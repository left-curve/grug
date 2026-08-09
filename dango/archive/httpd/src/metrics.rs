//! Metric names + descriptions for the read-API httpd — end-to-end HTTP request
//! stats (the per-feed SQL latencies live in the projection crate).
//!
//! See `design/observability.md`. [`init_metrics`] is always present (a no-op
//! without the `metrics` feature) and is called once at boot by the cli.

// The name constants are referenced only from `#[cfg(feature = "metrics")]`
// code, so a metrics-off build sees them as unused — expected, not a defect.
#![cfg_attr(not(feature = "metrics"), allow(dead_code))]

/// HTTP requests served (counter).
pub(crate) const HTTP_REQUESTS: &str = "archive_http_requests_total";
/// End-to-end HTTP request handling (histogram, seconds).
pub(crate) const HTTP_REQUEST_DURATION: &str = "archive_http_request_duration_seconds";
/// Concurrent in-flight HTTP requests (gauge).
pub(crate) const HTTP_IN_FLIGHT: &str = "archive_http_in_flight";

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
            describe_counter!(HTTP_REQUESTS, "HTTP requests served");
            describe_histogram!(HTTP_REQUEST_DURATION, "End-to-end HTTP request, seconds");
            describe_gauge!(HTTP_IN_FLIGHT, "Concurrent in-flight HTTP requests");
        });
    }
}
