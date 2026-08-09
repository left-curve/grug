//! Build the configured [`BlockSource`] for the `start` command.
//!
//! Only the deployment-specific endpoints / paths come from
//! [`BlockSourceConfig`]; the source's tuning knobs use the block-source
//! crate's own [`RemoteBlockSourceConfig`] / [`SentinelFetcherConfig`]
//! defaults. The result is an `Arc<dyn BlockSource>` — the app, committer, and
//! read schema never learn which concrete source they got.

use {
    crate::{
        config::{BlockSourceConfig, FetcherConfig, LocalSourceConfig, RemoteSourceConfig},
        home_directory::HomeDirectory,
    },
    anyhow::Context,
    dango_archive_block_source::{
        BlockFetcher, BlockSource, HttpdClient, LocalBlockSource, RemoteBlockSource,
        RemoteBlockSourceConfig, RocksdbBlockStore, SentinelBlockFetcher, SentinelFetcherConfig,
    },
    dango_indexer_cache::IndexerPath,
    std::sync::Arc,
};

/// Live-tail broadcast ring for the local source — matches the remote default
/// (`RemoteBlockSourceConfig::default().pubsub_buffer_size`).
const LOCAL_PUBSUB_BUFFER: usize = 2_000;

/// Build the `Arc<dyn BlockSource>` the app supervises, from config.
pub fn build(
    cfg: &BlockSourceConfig,
    home: &HomeDirectory,
) -> anyhow::Result<Arc<dyn BlockSource>> {
    match cfg {
        BlockSourceConfig::Remote(remote) => build_remote(remote, home),
        BlockSourceConfig::Local(local) => build_local(local, home),
    }
}

/// V2: a local RocksDB store, a live tail from `live_url`, and the configured
/// backfill fetcher (which, for `sentinel`, reuses the same `live_url` client).
fn build_remote(
    cfg: &RemoteSourceConfig,
    home: &HomeDirectory,
) -> anyhow::Result<Arc<dyn BlockSource>> {
    let store_path = home.resolve(&cfg.store_path);
    let store = Arc::new(
        RocksdbBlockStore::open(&store_path)
            .with_context(|| format!("failed to open block store at {}", store_path.display()))?,
    );

    // One node-httpd client: the live tail uses it directly, and the sentinel
    // fetcher reuses the same URL (`HttpdClient` is `Clone`), so the fetcher
    // carries no URL of its own.
    let live = HttpdClient::new(cfg.live_url.as_str())
        .with_context(|| format!("invalid live_url `{}`", cfg.live_url))?;

    let fetcher: Arc<dyn BlockFetcher> = match &cfg.fetcher {
        FetcherConfig::Sentinel { parallelism } => {
            // The block-source crate owns the tuning defaults; the config only
            // overrides what the deployment sets explicitly.
            let mut fetcher_config = SentinelFetcherConfig::default();
            if let Some(parallelism) = parallelism {
                fetcher_config.parallelism = *parallelism;
            }

            Arc::new(SentinelBlockFetcher::new(live.clone(), fetcher_config))
        },
    };

    Ok(Arc::new(RemoteBlockSource::new(
        store,
        live,
        fetcher,
        RemoteBlockSourceConfig::default(),
    )))
}

/// V1: co-located with a dango node — reads its on-disk indexer cache and tails
/// its httpd.
fn build_local(
    cfg: &LocalSourceConfig,
    home: &HomeDirectory,
) -> anyhow::Result<Arc<dyn BlockSource>> {
    let cache_path = home.resolve(&cfg.cache_path);
    let source = LocalBlockSource::new(
        IndexerPath::new_with_dir(cache_path),
        cfg.node_url.as_str(),
        LOCAL_PUBSUB_BUFFER,
    )
    .with_context(|| format!("invalid node_url `{}`", cfg.node_url))?;
    Ok(Arc::new(source))
}
