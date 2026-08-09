//! A generic, in-memory "recent blocks" stream: a bounded ring of the last `N`
//! per-block items plus a live broadcast tail, with a reliable subscription
//! builder that delivers a connect-time snapshot followed by the live tail,
//! in strict block-height order, with no silent drops.
//!
//! This is the reusable core behind the `perps_events` subscription. The
//! future "new blocks" subscription (see crate docs) is a second instantiation
//! `RecentStream<BlockAndOutcome>` — it drops onto this same primitive.
//!
//! Design notes (why this avoids the `event_by_addresses` failure modes):
//!
//! - The producer calls [`RecentStream::append`] INLINE and IN STRICT HEIGHT
//!   ORDER (one per block, including blocks with no matching events, so heights
//!   stay contiguous). There is no per-block task race, so the broadcast never
//!   publishes out of order.
//! - A subscriber's watermark advances ONLY to a height it has actually
//!   processed (via the snapshot or a live item), never blindly to a doorbell
//!   value, and never backward — so a block is never skipped unread.
//! - `broadcast::error::RecvError::Lagged` is surfaced, not swallowed. The
//!   broadcast buffer and the ring share the same capacity `N`: a consumer that
//!   is at most `N` blocks behind catches up through the broadcast's own
//!   buffered `Ok` delivery; one that falls further behind gets an explicit
//!   [`ResyncRequired`] (the in-memory window cannot serve it — deep history
//!   lives on the indexer node), rather than a silent hole.

use {
    dango_primitives::FullBlock,
    futures_util::stream::Stream,
    std::{
        collections::VecDeque,
        fmt,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
    },
    tokio::sync::broadcast,
};

/// A value stored in a [`RecentStream`] that knows the block height it belongs
/// to. One value per block height.
pub trait HasHeight: Send + Sync + 'static {
    fn height(&self) -> u64;
}

impl HasHeight for FullBlock {
    fn height(&self) -> u64 {
        self.block.info.height
    }
}

/// Returned when a subscriber requests — or falls behind to — a block height
/// older than the oldest block still retained in the ring. The client must
/// reconnect (optionally with a newer cursor); the ephemeral in-memory window
/// cannot serve the requested range. Deep history lives on the indexer node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResyncRequired {
    /// The earliest block height the subscriber still needs.
    pub requested_from: u64,

    /// The earliest block height still retained in the ring.
    pub ring_floor: u64,
}

impl fmt::Display for ResyncRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "resync required: requested from block {} but the oldest retained block is {}",
            self.requested_from, self.ring_floor
        )
    }
}

impl std::error::Error for ResyncRequired {}

// ---------------------------------- stream -----------------------------------

/// A cheaply-clonable handle (the clone shares one ring + broadcast) to an
/// in-memory recent-block stream.
pub struct RecentStream<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Clone for RecentStream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct Inner<T> {
    /// Ring of the last `capacity` per-block items, ordered oldest -> newest by
    /// height. Guarded by a std `Mutex` held only for O(1) push / synchronous
    /// clone-scan — never across an `.await`.
    ring: Mutex<VecDeque<Arc<T>>>,

    /// Max blocks retained in the ring AND the broadcast channel capacity. A
    /// consumer at most `capacity` blocks behind catches up via the broadcast;
    /// further behind => `ResyncRequired`.
    capacity: usize,

    /// Highest height ever appended; lets a fresh subscriber learn the tip
    /// without locking the ring.
    tip: AtomicU64,
    has_tip: AtomicBool,

    tx: broadcast::Sender<Arc<T>>,
}

impl<T> Inner<T>
where
    T: HasHeight,
{
    fn tip(&self) -> Option<u64> {
        if self.has_tip.load(Ordering::Acquire) {
            Some(self.tip.load(Ordering::Acquire))
        } else {
            None
        }
    }

    /// Clone every retained block whose height is in `lo..=hi`, ascending.
    /// Errors if the bottom of the requested range was already evicted.
    fn read_range(&self, lo: u64, hi: u64) -> Result<Vec<Arc<T>>, ResyncRequired> {
        if lo > hi {
            return Ok(Vec::new());
        }

        let ring = self.ring.lock().unwrap();

        if let Some(front) = ring.front() {
            let floor = front.height();
            if lo < floor {
                return Err(ResyncRequired {
                    requested_from: lo,
                    ring_floor: floor,
                });
            }
        }

        Ok(ring
            .iter()
            .filter(|item| {
                let h = item.height();
                h >= lo && h <= hi
            })
            .cloned()
            .collect())
    }
}

impl<T> RecentStream<T>
where
    T: HasHeight,
{
    /// `capacity` is the number of recent blocks retained in memory — both the
    /// reconnect/recovery window and the broadcast buffer.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (tx, _rx) = broadcast::channel(capacity);

        Self {
            inner: Arc::new(Inner {
                ring: Mutex::new(VecDeque::with_capacity(capacity)),
                capacity,
                tx,
                tip: AtomicU64::new(0),
                has_tip: AtomicBool::new(false),
            }),
        }
    }

    /// Append one per-block item and broadcast it to live subscribers.
    ///
    /// MUST be called in strict, gap-free height order by a single producer
    /// (the indexer's `post_indexing`, which the app awaits per height). A
    /// non-monotonic or duplicate height is a bug; it is logged and skipped
    /// rather than allowed to corrupt the ring ordering.
    pub fn append(&self, item: Arc<T>) {
        let height = item.height();

        {
            let mut ring = self.inner.ring.lock().unwrap();

            if let Some(last) = ring.back()
                && height <= last.height()
            {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    height,
                    last = last.height(),
                    "non-monotonic append to RecentStream; skipping"
                );

                return;
            }

            ring.push_back(item.clone());

            while ring.len() > self.inner.capacity {
                ring.pop_front();
            }
        }

        self.inner.tip.store(height, Ordering::Release);
        self.inner.has_tip.store(true, Ordering::Release);

        // `Err` only means there are no live subscribers — fine, the ring still
        // retains the block for future subscribers and reconnects.
        let _ = self.inner.tx.send(item);
    }

    /// Highest height ever appended (`None` if nothing has been appended).
    pub fn tip(&self) -> Option<u64> {
        self.inner.tip()
    }

    /// Lowest height still retained in the ring (`None` if empty).
    pub fn floor(&self) -> Option<u64> {
        self.inner.ring.lock().unwrap().front().map(|i| i.height())
    }

    /// Build a reliable subscription.
    ///
    /// - `since`: `None` streams only blocks newer than the current tip (live
    ///   from now). `Some(h)` additionally backfills retained blocks from `h`.
    /// - `filter`: applied to every block (snapshot and live). Returns the
    ///   client-facing projection of that block, or `None` to suppress a block
    ///   with no matching content (no empty items are emitted). The watermark
    ///   advances over suppressed blocks all the same, so they are never
    ///   re-examined.
    ///
    /// Returns `Err(ResyncRequired)` at connect time if `since` predates the
    /// retained window. A mid-stream lag past the window ends the stream
    /// (logged); the client should reconnect with a newer `since`.
    pub fn subscribe<R, F>(
        &self,
        since: Option<u64>,
        filter: F,
    ) -> Result<impl Stream<Item = R> + Send + 'static, ResyncRequired>
    where
        F: Fn(&T) -> Option<R> + Send + 'static,
        R: Send + 'static,
    {
        // Subscribe to the live tail FIRST, so any block appended between here
        // and the snapshot read below is buffered for us (closing the
        // snapshot/live gap). Overlap is de-duplicated by the watermark.
        let mut rx = self.inner.tx.subscribe();
        let inner = self.inner.clone();

        // Snapshot the ring (and validate `since` against the floor).
        let (snapshot, mut watermark): (Vec<Arc<T>>, Option<u64>) = {
            let ring = inner.ring.lock().unwrap();
            let floor = ring.front().map(|i| i.height());
            let tip = ring.back().map(|i| i.height());

            match since {
                Some(from) => {
                    if let Some(fl) = floor
                        && from < fl
                    {
                        return Err(ResyncRequired {
                            requested_from: from,
                            ring_floor: fl,
                        });
                    }

                    let snap = ring
                        .iter()
                        .filter(|i| i.height() >= from)
                        .cloned()
                        .collect::<Vec<_>>();

                    // If the snapshot is empty (e.g. `from` is in the future),
                    // start the live phase from `from`.
                    (snap, Some(from.saturating_sub(1)))
                },

                // Live-only: no historical backfill. `None` watermark means the
                // first live block establishes the baseline (no spurious gap on
                // a cold, empty ring).
                None => (Vec::new(), tip),
            }
        };

        let stream = async_stream::stream! {
            // Snapshot phase: emit matching blocks; advance the watermark over
            // the whole snapshot range (suppressed blocks included).
            let snapshot_tip = snapshot.last().map(|b| b.height());
            for item in snapshot {
                if let Some(projected) = filter(item.as_ref()) {
                    yield projected;
                }
            }
            if let Some(t) = snapshot_tip {
                watermark = Some(t);
            }

            // Live phase.
            loop {
                match rx.recv().await {
                    Ok(item) => {
                        let h = item.height();

                        match watermark {
                            None => {
                                // Cold start with `since = None`: this first
                                // block sets the baseline.
                                if let Some(projected) = filter(item.as_ref()) {
                                    yield projected;
                                }

                                watermark = Some(h);
                            },
                            Some(w) => {
                                if h <= w {
                                    // Already delivered (snapshot/live overlap)
                                    // or a stale re-send; never move backward.
                                    continue;
                                }

                                // Defensive: a gap in the Ok stream only arises
                                // if the producer skipped a height (it should
                                // not). Backfill what the ring still holds.
                                if h > w + 1 {
                                    match inner.read_range(w + 1, h - 1) {
                                        Ok(blocks) => {
                                            for b in blocks {
                                                if let Some(projected) = filter(b.as_ref()) {
                                                    yield projected;
                                                }
                                            }
                                        },
                                        Err(_resync) => {
                                            #[cfg(feature = "tracing")]
                                            tracing::warn!(%_resync, "ending subscription: gap older than retained window");

                                            return;
                                        },
                                    }
                                }

                                if let Some(projected) = filter(item.as_ref()) {
                                    yield projected;
                                }

                                watermark = Some(h);
                            },
                        }
                    },
                    Err(broadcast::error::RecvError::Lagged(_n)) => {
                        // We fell behind the broadcast buffer. Recover the
                        // skipped range from the ring, or resync if it has
                        // already been evicted past what we need.
                        #[cfg(feature = "metrics")]
                        metrics::counter!("indexer_stream.subscription.lagged.total").increment(1);

                        let tip = match inner.tip() {
                            Some(t) => t,
                            None => continue,
                        };

                        let from = match watermark {
                            Some(w) => w + 1,
                            // Cold start that lagged before any block: adopt the
                            // current tip as the baseline and carry on.
                            None => {
                                watermark = Some(tip);
                                continue;
                            },
                        };

                        match inner.read_range(from, tip) {
                            Ok(blocks) => {
                                for b in blocks {
                                    if let Some(projected) = filter(b.as_ref()) {
                                        yield projected;
                                    }
                                }

                                watermark = Some(tip);
                            },
                            Err(_resync) => {
                                #[cfg(feature = "tracing")]
                                tracing::warn!(%_resync, "ending subscription: lagged past retained window");

                                return;
                            },
                        }
                    },
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        };

        Ok(stream)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {super::*, futures_util::stream::StreamExt};

    #[derive(Debug, PartialEq, Eq)]
    struct TestBlock {
        height: u64,
    }

    impl HasHeight for TestBlock {
        fn height(&self) -> u64 {
            self.height
        }
    }

    fn block(height: u64) -> Arc<TestBlock> {
        Arc::new(TestBlock { height })
    }

    // Identity projection: every block matches, projecting to its height.
    fn id(b: &TestBlock) -> Option<u64> {
        Some(b.height)
    }

    #[tokio::test]
    async fn snapshot_then_live_in_order_no_duplicates() {
        let rs = RecentStream::new(10);
        rs.append(block(1));
        rs.append(block(2));
        rs.append(block(3));

        let stream = rs.subscribe(Some(1), id).unwrap();

        rs.append(block(4));
        rs.append(block(5));

        let got: Vec<u64> = stream.take(5).collect().await;
        // Snapshot 1,2,3 (rx cursor starts after 3, so live never re-sends
        // them) then live 4,5 — strictly ascending, no duplicates.
        assert_eq!(got, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn live_only_when_since_is_none() {
        let rs = RecentStream::new(10);
        rs.append(block(1));
        rs.append(block(2));

        let stream = rs.subscribe(None, id).unwrap();

        rs.append(block(3));
        rs.append(block(4));

        // No historical backfill: only blocks after the connect-time tip.
        let got: Vec<u64> = stream.take(2).collect().await;
        assert_eq!(got, vec![3, 4]);
    }

    #[tokio::test]
    async fn eviction_advances_floor_and_tip() {
        let rs = RecentStream::new(3);
        for h in 1..=5 {
            rs.append(block(h));
        }
        assert_eq!(rs.floor(), Some(3));
        assert_eq!(rs.tip(), Some(5));
    }

    #[tokio::test]
    async fn since_older_than_window_is_resync_at_connect() {
        let rs = RecentStream::new(3);
        for h in 1..=5 {
            rs.append(block(h));
        }
        // Ring holds 3,4,5; asking from 2 cannot be served.
        let err = rs.subscribe(Some(2), id).err().unwrap();
        assert_eq!(
            err,
            ResyncRequired {
                requested_from: 2,
                ring_floor: 3,
            }
        );
    }

    #[tokio::test]
    async fn lagging_past_window_ends_stream() {
        let rs = RecentStream::new(4);
        for h in 1..=4 {
            rs.append(block(h));
        }

        let stream = rs.subscribe(Some(1), id).unwrap();

        // Burst far past the broadcast buffer (and ring) without polling, so
        // the receiver lags and the skipped range is already evicted.
        for h in 5..=20 {
            rs.append(block(h));
        }

        // The snapshot (1..=4) is delivered; the live phase then hits Lagged,
        // finds the gap evicted, and ends the stream — no silent skip.
        let got: Vec<u64> = stream.collect().await;
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn slow_consumer_within_window_drops_nothing() {
        // The failure mode of `event_by_addresses`: a subscriber that is behind
        // when blocks are produced has its watermark advanced past blocks it
        // never read, silently losing them. `RecentStream` must NOT do that — a
        // consumer that lags by less than the window catches up through the
        // broadcast buffer and receives every block, in order, none dropped.
        let rs = RecentStream::new(8);
        let stream = rs.subscribe(None, id).unwrap();

        // Produce a full window's worth of blocks before the stream is ever
        // polled (the subscriber is "behind"), all within the buffer.
        for h in 1..=8 {
            rs.append(block(h));
        }

        let got: Vec<u64> = stream.take(8).collect().await;
        assert_eq!(got, (1..=8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn filter_suppresses_non_matching_blocks() {
        let rs = RecentStream::new(10);
        for h in 1..=5 {
            rs.append(block(h));
        }

        // Keep only even heights; odd blocks are suppressed but the watermark
        // still advances over them.
        let stream = rs
            .subscribe(Some(1), |b: &TestBlock| {
                b.height.is_multiple_of(2).then_some(b.height)
            })
            .unwrap();

        let got: Vec<u64> = stream.take(2).collect().await;
        assert_eq!(got, vec![2, 4]);
    }
}
