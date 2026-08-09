//! Batch block hydration over [`BlockSource::get`].
//!
//! A REST feed returns a whole page at once, so — unlike the GraphQL resolvers,
//! which hydrated a unit's raw payload (the full `Tx`, its `TxOutcome`,
//! non-priority event data) one resolution tick at a time — the read handlers
//! know every height the page needs up front. [`load_blocks`] collapses that to
//! one decode per distinct block: dedup the heights, fetch them concurrently,
//! and hand each whole block back behind an [`Arc`] so every row that lives in
//! it clones a handle, not the payload. Blocks are immutable, so the returned
//! map is always coherent.

use {
    crate::BlockSource,
    dango_archive_types::BlockData,
    std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    },
};

/// Load the blocks at `heights` from `source`, keyed by height. Heights are
/// deduplicated first, then fetched concurrently.
///
/// Each height is isolated: one the source cannot supply (`Ok(None)`) or that
/// *errors* is simply absent from the map, so a caller reading it back sees the
/// same "not available" as a miss — one unreadable block can't fail the sibling
/// rows of a page. A hard error is counted and logged so it stays visible.
#[must_use = "the loaded blocks are the hydration input"]
pub async fn load_blocks<I>(
    source: &Arc<dyn BlockSource>,
    heights: I,
) -> HashMap<u64, Arc<BlockData>>
where
    I: IntoIterator<Item = u64>,
{
    let distinct: HashSet<u64> = heights.into_iter().collect();

    let loads = distinct.into_iter().map(|height| {
        let source = Arc::clone(source);
        async move { (height, source.get(height).await) }
    });
    let results = futures::future::join_all(loads).await;

    let mut blocks = HashMap::with_capacity(results.len());
    for (height, result) in results {
        match result {
            Ok(Some(block)) => {
                blocks.insert(height, Arc::new(block));
            },
            Ok(None) => { /* missing — absent from the map, reads back as None */ },
            Err(_err) => {
                #[cfg(feature = "metrics")]
                metrics::counter!(crate::metrics::LOADER_FAILURES).increment(1);
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    height,
                    error = %_err,
                    "block load failed; treating as unavailable",
                );
            },
        }
    }
    blocks
}
