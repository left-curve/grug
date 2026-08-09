//! Metric names + descriptions for the block source.
//!
//! Names follow the `archive_*` flat-snake_case convention; the
//! RocksDB-internals gauges (emitted from [`store::disk`](crate::remote)) instead
//! reuse the node's `rocksdb_*` names verbatim, so the dango RocksDB Grafana
//! dashboard works against this service's `/metrics` unchanged. See
//! `design/observability.md`.
//!
//! [`init_metrics`] is always present (a no-op without the `metrics` feature) and
//! is called once at boot by the cli, alongside the other crates' init.

// The name constants are referenced only from `#[cfg(feature = "metrics")]`
// blocks, so a metrics-off build sees them as unused — expected, not a defect.
#![cfg_attr(not(feature = "metrics"), allow(dead_code))]

// ---- frontier & progress (local + remote) ----

/// Highest contiguous height reachable through the source (gauge).
pub(crate) const FRONTIER: &str = "archive_block_source_frontier";
/// Highest height seen on the live subscription — the observed tip (gauge).
pub(crate) const LIVE_HEIGHT: &str = "archive_block_source_live_height";
/// Blocks received from the live subscription (counter).
pub(crate) const LIVE_BLOCKS: &str = "archive_block_source_live_blocks_total";
/// Frontier-advance broadcasts emitted to projections (counter).
pub(crate) const BROADCAST_SENT: &str = "archive_block_source_broadcast_sent_total";
/// Live re-subscribes, by `reason` (counter).
pub(crate) const RECONNECTS: &str = "archive_block_source_reconnects_total";
/// Live-tail holes detected (counter).
pub(crate) const DISCONTINUITIES: &str = "archive_block_source_discontinuities_total";

// ---- healer / backfill (remote) ----

/// A gap backfill is in progress, 0/1 (gauge).
pub(crate) const HEALING: &str = "archive_block_source_healing";
/// Lowest missing height — the gap `from`; 0 when gap-free (gauge).
pub(crate) const GAP_LOW: &str = "archive_block_source_gap_low";
/// Top of the lowest gap — the gap `to`; 0 when gap-free (gauge).
pub(crate) const GAP_HIGH: &str = "archive_block_source_gap_high";
/// Gap contiguity-check failures (counter).
pub(crate) const BACKFILL_FAILURES: &str = "archive_block_source_backfill_failures_total";

// ---- fetcher (sentinel) ----

/// Blocks delivered by the fetcher — `rate()` is the recovery speed (counter).
pub(crate) const FETCHER_BLOCKS: &str = "archive_block_fetcher_blocks_total";
/// `/block/full/range` calls, by `outcome` (counter).
pub(crate) const FETCHER_REQUESTS: &str = "archive_block_fetcher_requests_total";
/// Per range HTTP call latency (histogram, seconds).
pub(crate) const FETCHER_REQUEST_DURATION: &str = "archive_block_fetcher_request_duration_seconds";

// ---- channels (fullness) ----

/// Queued items per channel, by `channel` (gauge).
pub(crate) const CHANNEL_DEPTH: &str = "archive_channel_depth";
/// Configured buffer size per channel, by `channel` (gauge).
pub(crate) const CHANNEL_CAPACITY: &str = "archive_channel_capacity";
/// Live broadcast subscribers (gauge).
pub(crate) const CHANNEL_RECEIVERS: &str = "archive_channel_receivers";

// ---- block store (RocksDB) operation latency ----

/// Persist + checkpoint one block — commit time (histogram, seconds).
pub(crate) const STORE_PUT_DURATION: &str = "archive_block_store_put_duration_seconds";
/// Point read + borsh decode — block read time (histogram, seconds).
pub(crate) const STORE_GET_DURATION: &str = "archive_block_store_get_duration_seconds";
/// Blocks written to the store (counter).
pub(crate) const STORE_BLOCKS_PERSISTED: &str = "archive_block_store_blocks_persisted_total";

// ---- block hydration ----

/// Block hydrations that errored and were served as unavailable (counter).
pub(crate) const LOADER_FAILURES: &str = "archive_block_loader_failures_total";

/// Register descriptions for this crate's metrics. Idempotent; a no-op unless the
/// `metrics` feature is on. Called once at boot.
pub fn init_metrics() {
    #[cfg(feature = "metrics")]
    {
        use {
            metrics::{describe_counter, describe_gauge, describe_histogram},
            std::sync::Once,
        };

        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            describe_gauge!(
                FRONTIER,
                "Highest contiguous height reachable through the source"
            );
            describe_gauge!(LIVE_HEIGHT, "Highest height seen on the live subscription");
            describe_counter!(LIVE_BLOCKS, "Blocks received from the live subscription");
            describe_counter!(BROADCAST_SENT, "Frontier-advance broadcasts emitted");
            describe_counter!(RECONNECTS, "Live subscription re-subscribes, by reason");
            describe_counter!(DISCONTINUITIES, "Live-tail holes detected");

            describe_gauge!(HEALING, "A gap backfill is in progress (0/1)");
            describe_gauge!(GAP_LOW, "Lowest missing height (gap from); 0 when gap-free");
            describe_gauge!(GAP_HIGH, "Top of the lowest gap (gap to); 0 when gap-free");
            describe_counter!(BACKFILL_FAILURES, "Gap contiguity-check failures");

            describe_counter!(FETCHER_BLOCKS, "Blocks delivered by the backfill fetcher");
            describe_counter!(FETCHER_REQUESTS, "Range-fetch calls, by outcome");
            describe_histogram!(
                FETCHER_REQUEST_DURATION,
                "Per range-fetch HTTP call, seconds"
            );

            describe_gauge!(CHANNEL_DEPTH, "Queued items per channel");
            describe_gauge!(CHANNEL_CAPACITY, "Configured buffer size per channel");
            describe_gauge!(CHANNEL_RECEIVERS, "Live broadcast subscribers");

            describe_histogram!(
                STORE_PUT_DURATION,
                "Persist + checkpoint one block, seconds"
            );
            describe_histogram!(STORE_GET_DURATION, "Read + decode one block, seconds");
            describe_counter!(STORE_BLOCKS_PERSISTED, "Blocks written to the store");

            describe_counter!(LOADER_FAILURES, "Block hydrations served as unavailable");
        });
    }
}
