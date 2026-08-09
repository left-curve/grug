use {
    super::{BlockStore, ranges::StoredRanges},
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData},
    std::{collections::BTreeMap, sync::Mutex},
};

/// In-memory [`BlockStore`] over a `BTreeMap` of blocks plus the
/// [`StoredRanges`] topology, both under one lock so a `put` updates them
/// together. For tests and ephemeral runs; a production run uses
/// [`RocksdbBlockStore`](super::RocksdbBlockStore), which additionally
/// checkpoints the topology to disk.
#[derive(Default)]
pub struct MemoryBlockStore {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    blocks: BTreeMap<u64, BlockData>,
    present: StoredRanges,
}

#[async_trait]
impl BlockStore for MemoryBlockStore {
    async fn put(&self, height: u64, data: &BlockData) -> AnyResult<Option<u64>> {
        let mut state = self.state.lock().unwrap();

        // Idempotent: a re-put leaves both the blocks and the topology untouched
        // and advances nothing.
        if state.blocks.contains_key(&height) {
            return Ok(None);
        }

        state.blocks.insert(height, data.clone());
        // `StoredRanges` owns the topology math and reports the frontier advance.
        Ok(state.present.insert(height))
    }

    async fn get(&self, height: u64) -> AnyResult<Option<BlockData>> {
        Ok(self.state.lock().unwrap().blocks.get(&height).cloned())
    }

    async fn contiguous_frontier(&self) -> AnyResult<Option<u64>> {
        Ok(self.state.lock().unwrap().present.contiguous_top())
    }

    async fn lowest_gap(&self) -> AnyResult<Option<(u64, u64)>> {
        Ok(self.state.lock().unwrap().present.first_gap())
    }
}
