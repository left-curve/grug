//! [`BlockSource`] trait and concrete implementations.
//!
//! - [`LocalBlockSource`] — V1 impl, co-located with the dango node.
//! - [`RemoteBlockSource`] — V2 impl, detached host; composes a
//!   [`BlockStore`], a node `HttpdClient` for the live tail, and a
//!   [`BlockFetcher`].
//! - [`BlockFetcher`] — bounded backfill abstraction; [`SentinelBlockFetcher`]
//!   pulls from a sentinel node.
//! - [`load_blocks`] — batch raw-payload hydration the REST read handlers run
//!   over a page's distinct heights.

mod httpd_client;
mod hydrate;
mod local;
mod metrics;
mod remote;
mod source;

pub use {
    crate::metrics::init_metrics,
    httpd_client::HttpdClient,
    hydrate::load_blocks,
    local::LocalBlockSource,
    remote::{
        BlockFetcher, BlockRangeClient, BlockStore, FetchStream, GENESIS_HEIGHT, MAX_BLOCK_RANGE,
        MemoryBlockStore, RemoteBlockSource, RemoteBlockSourceConfig, RocksdbBlockStore,
        SentinelBlockFetcher, SentinelFetcherConfig,
    },
    source::BlockSource,
};
