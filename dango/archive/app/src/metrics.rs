//! Metric names + descriptions for the app crate — the per-projection sync
//! state the supervisor and committer see from one place.
//!
//! See `design/observability.md`. [`init_metrics`] is always present (a no-op
//! without the `metrics` feature) and is called once at boot by the cli.

// The name constants are referenced only from `#[cfg(feature = "metrics")]`
// code, so a metrics-off build sees them as unused — expected, not a defect.
#![cfg_attr(not(feature = "metrics"), allow(dead_code))]

/// Last committed height per projection, by `projection` (gauge). The
/// per-projection sync anchor; lag = `block_source_frontier − this`.
pub(crate) const PROJECTION_HEIGHT: &str = "archive_projection_height";
/// Blocks committed per projection, by `projection` (counter) — `rate()` is the
/// catch-up / live throughput.
pub(crate) const PROJECTION_BLOCKS: &str = "archive_projection_blocks_total";
/// Time staging one block (`process`), by `projection` (histogram, seconds).
pub(crate) const PROJECTION_PROCESS_DURATION: &str = "archive_projection_process_duration_seconds";
/// Time committing one block (CH flush + PG tx), by `projection` (histogram).
pub(crate) const PROJECTION_COMMIT_DURATION: &str = "archive_projection_commit_duration_seconds";
/// Broadcast-overflow fallbacks to Phase-1 catch-up, by `projection` (counter) —
/// a rising count means the broadcast ring is under-sized for that projection.
pub(crate) const PROJECTION_LAGGED: &str = "archive_projection_lagged_total";

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
            describe_gauge!(PROJECTION_HEIGHT, "Last committed height, per projection");
            describe_counter!(PROJECTION_BLOCKS, "Blocks committed, per projection");
            describe_histogram!(
                PROJECTION_PROCESS_DURATION,
                "Time staging one block, seconds"
            );
            describe_histogram!(
                PROJECTION_COMMIT_DURATION,
                "Time committing one block, seconds"
            );
            describe_counter!(PROJECTION_LAGGED, "Broadcast-overflow catch-up fallbacks");
        });
    }
}
