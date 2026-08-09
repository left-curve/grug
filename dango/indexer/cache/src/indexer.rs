use {
    crate::{Context, cache_file::CacheFile, error::Result, indexer_path::IndexerPath},
    dango_primitives::BlockAndBlockOutcomeWithHttpDetails,
    serde::{Deserialize, Serialize},
    std::{
        collections::HashMap,
        io::Write,
        path::PathBuf,
        sync::{Arc, Mutex},
    },
};

#[cfg(feature = "http-request-details")]
use dango_primitives::{Hash256, HttpRequestDetails};

const HIGHEST_BLOCK_FILENAME: &str = "last_block.json";

// TODO: need to add `keep_blocks` configuration to allow choosing if we keep blocks
// or not, to save disk space. `app.toml` could also add a u64 field to limit the
// number of blocks to keep, deleting the oldest ones when exceeding that number.

#[derive(Clone, Default)]
pub struct Cache {
    pub context: Context,
    // This because the way indexer methods are called, we need to store the blocks
    // in memory between `pre_indexing`, `index_block` and `post_indexing`.
    blocks: Arc<Mutex<HashMap<u64, BlockAndBlockOutcomeWithHttpDetails>>>,
}

#[derive(Serialize, Deserialize)]
struct LastBlockHeight {
    block_height: u64,
}

impl Cache {
    pub fn new_with_tempdir() -> Self {
        let indexer_path = IndexerPath::new_with_tempdir();

        Self {
            context: Context {
                indexer_path,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn new_with_dir(directory: PathBuf) -> Self {
        let indexer_path = IndexerPath::new_with_dir(directory);

        Self {
            context: Context {
                indexer_path,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Set HTTP request details for transactions in the given block, those details
    /// are previously stored in the context by the httpd
    #[cfg(feature = "http-request-details")]
    fn set_http_request_details(
        &self,
        block: &dango_primitives::Block,
    ) -> dango_app::IndexerResult<HashMap<Hash256, HttpRequestDetails>> {
        let mut http_request_details: HashMap<Hash256, HttpRequestDetails> = HashMap::new();

        let mut transaction_hash_details = self
            .context
            .transactions_http_request_details
            .lock()
            .map_err(|_| dango_app::IndexerError::mutex_poisoned())?;

        http_request_details.extend(block.txs.iter().filter_map(|tx| {
            transaction_hash_details
                .remove(&tx.1)
                .map(|details| (tx.1, details))
        }));

        transaction_hash_details.clean();

        #[cfg(feature = "metrics")]
        metrics::gauge!("indexer.http_request_details.total")
            .set(transaction_hash_details.len() as f64);

        drop(transaction_hash_details);

        Ok(http_request_details)
    }

    /// Store the last block height in the file cache.
    fn store_last_block_height(context: &Context, block_height: u64, filename: &str) -> Result<()> {
        // We don't store if existing block height is greater
        if let Some(existing_block_height) = Self::read_last_block_height(context, filename)?
            && existing_block_height >= block_height
        {
            return Ok(());
        }

        let dir = context.indexer_path.blocks_path();
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        let payload = LastBlockHeight { block_height };
        serde_json::to_writer(&mut tmp, &payload)?;
        tmp.flush()?;
        tmp.persist(context.indexer_path.blocks_path().join(filename))?;

        Ok(())
    }

    /// Read the last block height from the file cache.
    fn read_last_block_height(context: &Context, filename: &str) -> Result<Option<u64>> {
        let path = context.indexer_path.blocks_path().join(filename);

        if !path.exists() {
            return Ok(None);
        }

        let file = std::fs::File::open(path)?;
        let payload: LastBlockHeight = serde_json::from_reader(file)?;

        Ok(Some(payload.block_height))
    }
}

impl Cache {
    pub async fn start(
        &mut self,
        _storage: &dyn dango_primitives::Storage,
    ) -> dango_app::IndexerResult<()> {
        // NOTE: might need to create caching directory, but working so far.
        Ok(())
    }

    pub async fn shutdown(&mut self) -> dango_app::IndexerResult<()> {
        Ok(())
    }

    /// No-op kept for symmetry with the other indexers' `wait_for_finish`.
    pub async fn wait_for_finish(&self) -> dango_app::IndexerResult<()> {
        Ok(())
    }

    /// Load a cached block from disk into the in-memory map, if a file exists
    /// at `block_path(block_height)`. Called during the indexer pipeline's
    /// `pre_indexing` phase, and during `reindex` to populate the in-memory
    /// hop that `post_indexing` later drains.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn pre_indexing(&self, block_height: u64) -> dango_app::IndexerResult<()> {
        let file_path = self.context.indexer_path.block_path(block_height);

        // This is used when reindexing existing blocks, since `index_block` won't be called.
        if CacheFile::exists(file_path.clone()) {
            let cache_file = CacheFile::load_from_disk(file_path)?;

            self.blocks
                .lock()
                .map_err(|_| dango_app::IndexerError::mutex_poisoned())?
                .insert(block_height, cache_file.data);
        }

        Ok(())
    }

    /// Persist a freshly minted block to disk (or reload it from disk if it
    /// was already there), and store the payload in the in-memory map for
    /// `post_indexing` to consume.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn index_block(
        &self,
        block: &dango_primitives::Block,
        block_outcome: &dango_primitives::BlockOutcome,
    ) -> dango_app::IndexerResult<()> {
        let file_path = self.context.indexer_path.block_path(block.info.height);

        if CacheFile::exists(file_path.clone()) {
            #[cfg(feature = "tracing")]
            tracing::info!(
                block_height = block.info.height,
                "Block already cached, skipping writing to disk",
            );

            // I have to reload the file to get the http-request-details,
            // which we lost since if we are not going through the httpd.
            let cache_file = CacheFile::load_from_disk(file_path)?;

            self.blocks
                .lock()
                .map_err(|_| dango_app::IndexerError::mutex_poisoned())?
                .insert(block.info.height, cache_file.data);
        } else {
            #[cfg(feature = "tracing")]
            tracing::info!(
                block_height = block.info.height,
                file_path = ?file_path,
                "Block will be saved to disk",
            );

            #[allow(unused_mut)]
            let mut cache_file = CacheFile::new(file_path, block.clone(), block_outcome.clone());

            #[cfg(feature = "http-request-details")]
            {
                cache_file.data.http_request_details = self.set_http_request_details(block)?;
            }
            cache_file.save_to_disk()?;

            self.blocks
                .lock()
                .map_err(|_| dango_app::IndexerError::mutex_poisoned())?
                .insert(block.info.height, cache_file.data);
        }

        Ok(())
    }

    /// Drain the in-memory map for `block_height`, finalize the on-disk file
    /// (compress + record last-block-height), and return the payload so
    /// downstream consumers (`SqlIndexer`, `ClickhouseIndexer`) can process it.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn post_indexing(
        &self,
        block_height: u64,
    ) -> dango_app::IndexerResult<BlockAndBlockOutcomeWithHttpDetails> {
        let Some(data) = self
            .blocks
            .lock()
            .map_err(|_| dango_app::IndexerError::mutex_poisoned())?
            .remove(&block_height)
        else {
            return Err(dango_app::IndexerError::hook(format!(
                "Block data for height {block_height} not found in cache indexer",
            )));
        };

        let file_path = self.context.indexer_path.block_path(block_height);

        if CacheFile::exists(file_path.clone()) {
            CacheFile::compress_file(file_path.clone())?;

            Self::store_last_block_height(&self.context, block_height, HIGHEST_BLOCK_FILENAME)?;
        }

        Ok(data)
    }
}
