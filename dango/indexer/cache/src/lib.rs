pub mod cache_file;
pub mod compress;
pub mod context;
pub mod error;
pub mod indexer;
pub mod indexer_path;
#[cfg(feature = "s3")]
pub mod retrieval;
#[cfg(feature = "s3")]
pub mod s3;

// ---- always available ----

pub use {
    compress::{
        BatchCompressor, BatchReader, BlockCompressor, Codec, Encode, Stored, decode_block,
    },
    context::Context,
    error::Result,
    indexer::Cache,
    indexer_path::IndexerPath,
};

// ---- feature-gated ----

#[cfg(feature = "xz-codec")]
pub use compress::Xz;
#[cfg(feature = "s3")]
pub use {
    retrieval::{BatchClient, StorageConfig},
    s3::{Client, S3Config},
};
