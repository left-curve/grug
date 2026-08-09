//! Metrics wiring for `start`: install the global Prometheus recorder (so every
//! crate's `metrics::*` macros record), register each crate's descriptions, and
//! — when enabled — serve the `/metrics` scrape endpoint.
//!
//! The recorder is installed unconditionally and as early as possible, so
//! metrics are captured even when the endpoint is disabled. The endpoint itself
//! reuses the node's `dango_indexer_metrics::run_metrics_server` and is
//! supervised by `start` alongside `App::run` (see `design/observability.md`).

use {
    crate::config::MetricsConfig,
    anyhow::Context,
    metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle},
};

/// Install the global recorder and register every crate's metric descriptions.
/// Call once, before any metric is emitted. The returned handle renders the
/// registry for the `/metrics` endpoint.
pub fn install() -> anyhow::Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .context("installing the Prometheus recorder")?;

    // Description-only registration (help text); idempotent, no-op without the
    // metrics feature (which the cli always enables on these crates).
    dango_archive_block_source::init_metrics();
    dango_archive_projection::init_metrics();
    dango_archive_app::init_metrics();
    dango_archive_httpd::init_metrics();

    // A constant build-info gauge carrying the deployed version + commit.
    metrics::describe_gauge!("archive_build_info", "Deployed build (always 1)");
    metrics::gauge!(
        "archive_build_info",
        "version" => env!("CARGO_PKG_VERSION"),
        "commit" => dango_primitives::GIT_COMMIT,
    )
    .set(1.0);

    Ok(handle)
}

/// Serve the `/metrics` scrape endpoint until the process stops. Awaited
/// directly by `start` (via `tokio::try_join!`, so the `!Send` actix server runs
/// on the current task with no extra thread — the same way the dango node drives
/// it). Only called when `cfg.enabled`.
pub async fn serve(cfg: &MetricsConfig, handle: PrometheusHandle) -> anyhow::Result<()> {
    dango_indexer_metrics::run_metrics_server(&cfg.ip, cfg.port, handle)
        .await
        .context("metrics httpd server stopped with an error")
}
