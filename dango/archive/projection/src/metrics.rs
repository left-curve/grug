//! Metric names + descriptions for the projection crate, plus the [`timed_query`]
//! helper that wraps a feed's database call in a per-`query` latency histogram.
//!
//! See `design/observability.md`. [`init_metrics`] is always present (a no-op
//! without the `metrics` feature) and is called once at boot by the cli.

// The name constants are referenced only from `#[cfg(feature = "metrics")]`
// code, so a metrics-off build sees them as unused — expected, not a defect.
#![cfg_attr(not(feature = "metrics"), allow(dead_code))]

// ---- activity projection write volume ----

/// `activity_transactions` rows staged per block (counter).
pub(crate) const ACTIVITY_TRANSACTIONS: &str = "archive_activity_transactions_total";
/// `activity_events` rows staged per block (counter).
pub(crate) const ACTIVITY_EVENTS: &str = "archive_activity_events_total";
/// `activity_event_data` rows staged per block (counter).
pub(crate) const ACTIVITY_EVENT_DATA: &str = "archive_activity_event_data_total";

// ---- read-query latency (shared with the block-source `block` query) ----

/// Per-feed database query latency, by `query` (histogram, seconds).
pub(crate) const QUERY_DURATION: &str = "archive_query_duration_seconds";
/// Per-feed query executions, by `query` + `outcome` (counter).
pub(crate) const QUERY_TOTAL: &str = "archive_query_total";

/// Time a feed's database call and record it under `query`, labelling the
/// outcome ok / error. The metrics-off build is a transparent passthrough, so
/// call sites need no feature gate: `timed_query("events_by_type", q.all(db)).await?`.
#[cfg(feature = "metrics")]
pub(crate) async fn timed_query<F, T, E>(query: &'static str, fut: F) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    let start = std::time::Instant::now();
    let result = fut.await;
    let outcome = if result.is_ok() {
        "ok"
    } else {
        "error"
    };
    metrics::histogram!(QUERY_DURATION, "query" => query).record(start.elapsed().as_secs_f64());
    metrics::counter!(QUERY_TOTAL, "query" => query, "outcome" => outcome).increment(1);
    result
}

/// Passthrough when metrics are compiled out — keeps the call sites gate-free.
#[cfg(not(feature = "metrics"))]
pub(crate) async fn timed_query<F, T, E>(_query: &'static str, fut: F) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    fut.await
}

/// Register descriptions for this crate's metrics. Idempotent; a no-op unless the
/// `metrics` feature is on. Called once at boot.
pub fn init_metrics() {
    #[cfg(feature = "metrics")]
    {
        use {
            metrics::{describe_counter, describe_histogram},
            std::sync::Once,
        };

        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            describe_counter!(
                ACTIVITY_TRANSACTIONS,
                "activity_transactions rows staged per block"
            );
            describe_counter!(ACTIVITY_EVENTS, "activity_events rows staged per block");
            describe_counter!(
                ACTIVITY_EVENT_DATA,
                "activity_event_data rows staged per block"
            );
            describe_histogram!(QUERY_DURATION, "Read-query latency, by query, seconds");
            describe_counter!(QUERY_TOTAL, "Read-query executions, by query and outcome");
        });
    }
}
