use {
    super::{BlockStore, ranges::StoredRanges},
    anyhow::anyhow,
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData},
    rocksdb::{
        BlockBasedIndexType, BlockBasedOptions, Cache, ColumnFamily, DB, DBCompressionType,
        Options, WriteBatch,
    },
    std::{
        path::Path,
        sync::{Arc, Mutex},
    },
};

/// The default CF holds metadata only (the topology checkpoint); named here
/// because `open_cf_with_opts` must list every column family explicitly.
const CF_DEFAULT: &str = "default";

/// Column family for the raw blocks: key = `height` big-endian (so RocksDB's
/// lexicographic order *is* height order), value = borsh-encoded [`BlockData`].
const CF_BLOCKS: &str = "blocks";

/// Key in the default CF holding the topology checkpoint — the [`StoredRanges`]
/// serialized as a borsh `Vec<(u64, u64)>`.
const TOPOLOGY_KEY: &[u8] = b"topology";

/// Persistent [`BlockStore`] over a local RocksDB.
///
/// Blocks live in the `blocks` CF keyed by big-endian height; the stored-height
/// topology is mirrored in RAM (for O(1) frontier/gap queries) and checkpointed
/// to the default CF in the **same atomic batch** as each block, so boot reads
/// the frontier in O(#ranges) rather than scanning every height. The raw blocks
/// are re-fetchable, so a missing checkpoint is recovered by rebuilding the
/// topology from the block keys.
///
/// Single-process by design: the detached `RemoteBlockSource` owns it, the
/// coordinator is the only writer, and the query service reaches blocks through
/// the same in-process source — so an embedded store is reachable, not a limit.
pub struct RocksdbBlockStore {
    /// `Arc` so each `put`/`get` can hand a cheap clone to `spawn_blocking`,
    /// keeping the synchronous RocksDB I/O off the async workers.
    db: Arc<DB>,
    /// In-RAM mirror of the durable topology; authoritative for queries.
    ///
    /// `std::sync::Mutex` is correct in this async store: the critical sections
    /// are synchronous (no `.await` held across the guard), and every disk
    /// access happens *outside* the lock.
    ranges: Mutex<StoredRanges>,
}

impl RocksdbBlockStore {
    /// Open (creating if absent) the store at `path` and load the topology —
    /// from the checkpoint, or by rebuilding it from the block keys if there is
    /// no checkpoint yet.
    pub fn open<P>(path: P) -> AnyResult<Self>
    where
        P: AsRef<Path>,
    {
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        // The default CF carries only the tiny topology checkpoint, so it needs
        // no tuning; the blocks CF carries the large payloads and is tuned for
        // them — see `blocks_cf_options`.
        let db = DB::open_cf_with_opts(
            &db_opts,
            path,
            [
                (CF_DEFAULT, Options::default()),
                (CF_BLOCKS, blocks_cf_options()),
            ],
        )?;

        let ranges = match db.get(TOPOLOGY_KEY)? {
            // A present checkpoint is the fast path. If it fails to decode
            // (corruption), fall back to the rebuild rather than failing boot
            // forever — the blocks are the source of truth, the checkpoint is
            // only an optimization.
            Some(bytes) => match borsh::from_slice::<Vec<(u64, u64)>>(&bytes) {
                Ok(checkpoint) => StoredRanges::from_ranges(&checkpoint),
                Err(_err) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        error = %_err,
                        "topology checkpoint is corrupt; rebuilding from block keys"
                    );
                    Self::rebuild_topology(&db)?
                },
            },
            None => Self::rebuild_topology(&db)?,
        };

        Ok(Self {
            db: Arc::new(db),
            ranges: Mutex::new(ranges),
        })
    }

    /// Reconstruct the topology by scanning the block keys — the recovery path
    /// when no checkpoint is present (a fresh DB is empty; a populated one
    /// without a checkpoint is a one-off rebuild).
    fn rebuild_topology(db: &DB) -> AnyResult<StoredRanges> {
        // Key-only scan: a `raw_iterator` resolves a blob only when `value()` is
        // called, so reading just the 8-byte height keys never touches the block
        // payloads. The high-level `iterator_cf` materializes `(key, value)`
        // pairs — with BlobDB that would fetch every payload off the blob files
        // (the whole archive, tens of TB at 100M blocks) only to discard it.
        let mut ranges = StoredRanges::default();
        let mut iter = db.raw_iterator_cf(blocks_cf(db));
        iter.seek_to_first();
        while iter.valid() {
            let key = iter
                .key()
                .ok_or_else(|| anyhow!("block iterator is valid but returned no key"))?;
            let bytes: [u8; 8] = key
                .try_into()
                .map_err(|_| anyhow!("block key is not 8 bytes: {key:?}"))?;
            ranges.insert(u64::from_be_bytes(bytes));
            iter.next();
        }
        // Surface an I/O error that ended the iteration (vs. a clean end).
        iter.status()?;
        Ok(ranges)
    }
}

#[async_trait]
impl BlockStore for RocksdbBlockStore {
    async fn put(&self, height: u64, data: &BlockData) -> AnyResult<Option<u64>> {
        // Single-writer (the coordinator): this read-then-clone is race-free.
        // Compute the would-be next topology on a clone so the RAM frontier is
        // not advanced until the block is durable.
        let (next, advance) = {
            let ranges = self.ranges.lock().unwrap();
            if ranges.contains(height) {
                return Ok(None); // idempotent
            }
            let mut next = ranges.clone();
            let advance = next.insert(height);
            (next, advance)
        };

        // Persist block + topology checkpoint atomically and durably, *before*
        // the advance is exposed — so `h <= frontier ⟹ get(h) = Some` holds even
        // across a crash. The RocksDB write runs on the blocking pool so it does
        // not stall an async worker; the block encode stays here because the
        // coordinator is the only writer, so at most one encode is in flight.
        //
        // Time the whole persist (encode + handoff + write) — the cost the
        // coordinator pays per block — and count it once durable.
        #[cfg(feature = "metrics")]
        let put_start = std::time::Instant::now();

        let db = self.db.clone();
        let block_bytes = borsh::to_vec(data)?;
        let topology_bytes = borsh::to_vec(&next.to_ranges())?;
        tokio::task::spawn_blocking(move || -> AnyResult<()> {
            let mut batch = WriteBatch::default();
            batch.put_cf(blocks_cf(&db), height.to_be_bytes(), block_bytes);
            batch.put(TOPOLOGY_KEY, topology_bytes);
            db.write(batch)?;
            Ok(())
        })
        .await??;

        // The block is now durable; commit the advanced topology to RAM.
        *self.ranges.lock().unwrap() = next;

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(crate::metrics::STORE_PUT_DURATION)
                .record(put_start.elapsed().as_secs_f64());
            metrics::counter!(crate::metrics::STORE_BLOCKS_PERSISTED).increment(1);
        }

        Ok(advance)
    }

    async fn get(&self, height: u64) -> AnyResult<Option<BlockData>> {
        // Read + decode on the blocking pool: both the RocksDB point read (a
        // blob fetch under BlobDB) and the borsh decode of a large payload are
        // synchronous and CPU-bound, and `get` is called concurrently by every
        // projection during catch-up — keeping them off the async workers is
        // what stops a backfill read storm from starving the coordinator.
        #[cfg(feature = "metrics")]
        let get_start = std::time::Instant::now();

        let db = self.db.clone();
        let block = tokio::task::spawn_blocking(move || -> AnyResult<Option<BlockData>> {
            match db.get_cf(blocks_cf(&db), height.to_be_bytes())? {
                Some(bytes) => Ok(Some(borsh::from_slice(&bytes)?)),
                None => Ok(None),
            }
        })
        .await?;

        #[cfg(feature = "metrics")]
        metrics::histogram!(crate::metrics::STORE_GET_DURATION)
            .record(get_start.elapsed().as_secs_f64());

        block
    }

    async fn contiguous_frontier(&self) -> AnyResult<Option<u64>> {
        Ok(self.ranges.lock().unwrap().contiguous_top())
    }

    async fn lowest_gap(&self) -> AnyResult<Option<(u64, u64)>> {
        Ok(self.ranges.lock().unwrap().first_gap())
    }

    /// Poll RocksDB's integer properties for both column families and emit them
    /// as gauges — the same shape as the dango node's `StatisticsWorker`, under
    /// the same `rocksdb_*` names (plus `rocksdb_blob_bytes` for the BlobDB
    /// payload files, which hold ~95% of the bytes here) so the node's RocksDB
    /// dashboard works against this service unchanged. A property the engine does
    /// not expose is silently skipped, never an error.
    fn sample_metrics(&self) {
        #[cfg(feature = "metrics")]
        {
            for cf_name in [CF_DEFAULT, CF_BLOCKS] {
                let Some(cf) = self.db.cf_handle(cf_name) else {
                    continue;
                };

                for (metric, props) in ROCKSDB_GAUGES {
                    for (property, type_label) in *props {
                        if let Ok(Some(v)) = self.db.property_int_value_cf(cf, *property) {
                            metrics::gauge!(*metric, "type" => *type_label, "cf" => cf_name)
                                .set(v as f64);
                        }
                    }
                }

                // SST files per LSM level — the iterator-cost signal.
                for level in 0..7u32 {
                    let property = format!("rocksdb.num-files-at-level{level}");
                    if let Ok(Some(v)) = self.db.property_int_value_cf(cf, property.as_str()) {
                        metrics::gauge!(
                            "rocksdb_lsm_count",
                            "type" => "num_files_at_level",
                            "level" => level.to_string(),
                            "cf" => cf_name,
                        )
                        .set(v as f64);
                    }
                }
            }
        }
    }
}

/// `(metric_name, &[(rocksdb_property, type_label)])` — the integer properties
/// sampled per column family, grouped into the dango `rocksdb_*` gauges. Raw
/// property strings (stable RocksDB names); an unknown one just yields no value.
#[cfg(feature = "metrics")]
const ROCKSDB_GAUGES: &[(&str, &[(&str, &str)])] = &[
    (
        "rocksdb_memtable_bytes",
        &[
            ("rocksdb.cur-size-active-mem-table", "cur_size_active"),
            ("rocksdb.cur-size-all-mem-tables", "cur_size_all"),
            ("rocksdb.size-all-mem-tables", "size_all"),
        ],
    ),
    (
        "rocksdb_sst_bytes",
        &[
            ("rocksdb.live-sst-files-size", "live_sst_files_size"),
            ("rocksdb.total-sst-files-size", "total_sst_files_size"),
            ("rocksdb.estimate-live-data-size", "estimate_live_data_size"),
        ],
    ),
    (
        "rocksdb_compaction_bytes",
        &[(
            "rocksdb.estimate-pending-compaction-bytes",
            "estimate_pending_compaction",
        )],
    ),
    (
        "rocksdb_block_cache_bytes",
        &[
            ("rocksdb.block-cache-capacity", "capacity"),
            ("rocksdb.block-cache-usage", "usage"),
            ("rocksdb.block-cache-pinned-usage", "pinned_usage"),
        ],
    ),
    // BlobDB payload files — the bulk of the on-disk bytes for this store.
    (
        "rocksdb_blob_bytes",
        &[
            ("rocksdb.total-blob-file-size", "total_blob_file_size"),
            ("rocksdb.live-blob-file-size", "live_blob_file_size"),
            (
                "rocksdb.live-blob-file-garbage-size",
                "live_blob_file_garbage_size",
            ),
        ],
    ),
    (
        "rocksdb_memtable_count",
        &[
            ("rocksdb.num-entries-active-mem-table", "entries_active"),
            ("rocksdb.num-entries-imm-mem-tables", "entries_imm"),
            ("rocksdb.num-deletes-active-mem-table", "deletes_active"),
            ("rocksdb.num-deletes-imm-mem-tables", "deletes_imm"),
        ],
    ),
    (
        "rocksdb_compaction_count",
        &[
            ("rocksdb.num-running-compactions", "running_compactions"),
            ("rocksdb.num-running-flushes", "running_flushes"),
        ],
    ),
    (
        "rocksdb_lsm_count",
        &[
            ("rocksdb.num-live-versions", "live_versions"),
            (
                "rocksdb.current-super-version-number",
                "super_version_number",
            ),
            ("rocksdb.num-blob-files", "num_blob_files"),
        ],
    ),
    (
        "rocksdb_errors_count",
        &[("rocksdb.background-errors", "background_errors")],
    ),
    (
        "rocksdb_flags",
        &[
            ("rocksdb.compaction-pending", "compaction_pending"),
            ("rocksdb.mem-table-flush-pending", "memtable_flush_pending"),
            ("rocksdb.is-write-stopped", "is_write_stopped"),
            (
                "rocksdb.is-file-deletions-enabled",
                "is_file_deletions_enabled",
            ),
        ],
    ),
];

/// Handle to the blocks CF — a cheap lookup; the CF always exists once opened.
fn blocks_cf(db: &DB) -> &ColumnFamily {
    db.cf_handle(CF_BLOCKS)
        .expect("blocks column family must exist")
}

/// Tuning for the blocks CF, shaped by the **measured** mainnet workload
/// (2026-06-24: ~32.5M blocks, growing at ~2.79 blk/s → ~100M in ~9 months;
/// `BlockData` payloads median ~15-25 KB, p90 ~150 KB borsh). Keys are fixed
/// 8-byte big-endian heights; values are large, immutable, compressible, and
/// written append-only. The fixed key size is not where the wins are — the
/// tuning targets the large values, the read path at 100M keys, and the writes.
fn blocks_cf_options() -> Options {
    let mut opts = Options::default();

    // Key-value separation (BlobDB): the large payloads live in blob files,
    // leaving the LSM tiny (key → blob pointer). Compaction then does not
    // rewrite the big values, and key scans (the topology rebuild) stay cheap.
    // At the measured sizes essentially every block exceeds `min_blob_size`, so
    // ~all payloads go to blobs and the LSM holds little beyond the pointers.
    opts.set_enable_blob_files(true);
    opts.set_min_blob_size(4 * 1024);
    opts.set_blob_compression_type(DBCompressionType::Zstd);
    // The payloads are ~95% of the bytes and were otherwise uncached. A
    // dedicated blob cache helps the query service and any projection that
    // re-reads recent blocks; a purely sequential backfill barely uses it (no
    // downside). Separate from the block cache so the two do not contend.
    opts.set_blob_cache(&Cache::new_lru_cache(512 * 1024 * 1024));

    // SST compression: fast lz4 on the churny upper levels, max-ratio zstd at the
    // stable bottom level where ~all of the archive settles.
    opts.set_compression_type(DBCompressionType::Lz4);
    opts.set_bottommost_compression_type(DBCompressionType::Zstd);

    // Append-only, no deletes: leveled compaction with dynamic level sizing
    // minimizes space amplification.
    opts.set_level_compaction_dynamic_level_bytes(true);

    // Larger memtable → fewer flushes during the backfill burst.
    opts.set_write_buffer_size(64 * 1024 * 1024);
    // Backfill writes far faster than the ~2.79 blk/s live rate; give compaction
    // and flushing enough threads to keep up and avoid L0 write stalls.
    opts.set_max_background_jobs(6);
    opts.set_max_subcompactions(4);

    // Read path. With BlobDB the SST data blocks are just pointers, so this
    // cache is almost entirely index+filter — and at 100M keys those alone are
    // ~350-450 MB (a 10-bit/key bloom is ~125 MB), so the old 256 MB would
    // thrash. 1 GiB gives headroom; raise it if the host has spare RAM.
    let mut block_opts = BlockBasedOptions::default();
    block_opts.set_block_size(16 * 1024);
    block_opts.set_bloom_filter(10.0, true);
    block_opts.set_block_cache(&Cache::new_lru_cache(1024 * 1024 * 1024));
    block_opts.set_cache_index_and_filter_blocks(true);
    // Partitioned index + filter: a monolithic per-SST filter at 100M keys is
    // huge; partitioning loads only the relevant slices per lookup. Pinning the
    // top level keeps the small upper index/filter resident while the partitions
    // stay cached on demand — so data blocks no longer evict them (this replaces
    // the unavailable `..._with_high_priority`). `format_version >= 5` is
    // required for partitioned filters.
    block_opts.set_index_type(BlockBasedIndexType::TwoLevelIndexSearch);
    block_opts.set_partition_filters(true);
    block_opts.set_format_version(5);
    block_opts.set_pin_top_level_index_and_filter(true);
    opts.set_block_based_table_factory(&block_opts);

    opts
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use {super::*, dango_archive_types::BlockDataExt, dango_temp_rocksdb::TempDataDir};

    fn block(height: u64) -> BlockData {
        use dango_primitives::{Block, BlockInfo, BlockOutcome, Hash256, Timestamp};
        BlockData {
            block: Block {
                info: BlockInfo {
                    height,
                    timestamp: Timestamp::from_nanos(0),
                    hash: Hash256::ZERO,
                },
                txs: vec![],
            },
            outcome: BlockOutcome {
                height,
                app_hash: Hash256::ZERO,
                cron_outcomes: vec![],
                tx_outcomes: vec![],
            },
        }
    }

    #[tokio::test]
    async fn put_get_roundtrip() {
        let dir = TempDataDir::new("_blockstore_roundtrip");
        let store = RocksdbBlockStore::open(&dir).unwrap();

        store.put(5, &block(5)).await.unwrap();
        assert_eq!(store.get(5).await.unwrap().unwrap().height(), 5);
        assert_eq!(store.get(6).await.unwrap(), None);
    }

    #[tokio::test]
    async fn topology_tracks_frontier_and_gap() {
        let dir = TempDataDir::new("_blockstore_topology");
        let store = RocksdbBlockStore::open(&dir).unwrap();

        for height in 1..=3 {
            store.put(height, &block(height)).await.unwrap();
        }
        assert_eq!(store.contiguous_frontier().await.unwrap(), Some(3));
        assert_eq!(store.lowest_gap().await.unwrap(), None);

        // An island above a gap: frontier unchanged, the hole is reported.
        store.put(5, &block(5)).await.unwrap();
        assert_eq!(store.contiguous_frontier().await.unwrap(), Some(3));
        assert_eq!(store.lowest_gap().await.unwrap(), Some((4, 4)));

        // Fill the hole: the frontier jumps across the island to 5.
        store.put(4, &block(4)).await.unwrap();
        assert_eq!(store.contiguous_frontier().await.unwrap(), Some(5));
        assert_eq!(store.lowest_gap().await.unwrap(), None);
    }

    #[tokio::test]
    async fn put_reports_bulk_advance_and_is_idempotent() {
        let dir = TempDataDir::new("_blockstore_advance");
        let store = RocksdbBlockStore::open(&dir).unwrap();

        assert_eq!(store.put(3, &block(3)).await.unwrap(), None); // island
        assert_eq!(store.put(1, &block(1)).await.unwrap(), Some(1)); // frontier 1
        assert_eq!(store.put(2, &block(2)).await.unwrap(), Some(3)); // bridges → 3
        assert_eq!(store.put(2, &block(2)).await.unwrap(), None); // re-put: no-op
    }

    #[tokio::test]
    async fn survives_reopen_from_checkpoint() {
        let dir = TempDataDir::new("_blockstore_reopen");
        {
            let store = RocksdbBlockStore::open(&dir).unwrap();
            for height in 1..=3 {
                store.put(height, &block(height)).await.unwrap();
            }
            store.put(5, &block(5)).await.unwrap(); // leaves a gap at 4
        }

        // Reopen the same path: the topology comes back from the checkpoint with
        // no scan, and the blocks are intact.
        let store = RocksdbBlockStore::open(&dir).unwrap();
        assert_eq!(store.contiguous_frontier().await.unwrap(), Some(3));
        assert_eq!(store.lowest_gap().await.unwrap(), Some((4, 4)));
        assert_eq!(store.get(2).await.unwrap().unwrap().height(), 2);
        assert_eq!(store.get(5).await.unwrap().unwrap().height(), 5);
    }

    #[tokio::test]
    async fn rebuilds_topology_when_checkpoint_absent() {
        let dir = TempDataDir::new("_blockstore_rebuild_absent");
        {
            let store = RocksdbBlockStore::open(&dir).unwrap();
            for height in [1, 2, 3, 5, 6] {
                store.put(height, &block(height)).await.unwrap();
            }
            // Drop the checkpoint so the next open must rebuild from the keys
            // (the key-only scan path).
            store.db.delete(TOPOLOGY_KEY).unwrap();
        }

        let store = RocksdbBlockStore::open(&dir).unwrap();
        assert_eq!(store.contiguous_frontier().await.unwrap(), Some(3));
        assert_eq!(store.lowest_gap().await.unwrap(), Some((4, 4)));
        assert_eq!(store.get(6).await.unwrap().unwrap().height(), 6);
    }

    #[tokio::test]
    async fn rebuilds_topology_when_checkpoint_is_corrupt() {
        let dir = TempDataDir::new("_blockstore_rebuild_corrupt");
        {
            let store = RocksdbBlockStore::open(&dir).unwrap();
            for height in 1..=3 {
                store.put(height, &block(height)).await.unwrap();
            }
            // Garble the checkpoint: the next open must fall back to the rebuild,
            // not fail boot.
            store.db.put(TOPOLOGY_KEY, b"not valid borsh").unwrap();
        }

        let store = RocksdbBlockStore::open(&dir).unwrap();
        assert_eq!(store.contiguous_frontier().await.unwrap(), Some(3));
        assert_eq!(store.lowest_gap().await.unwrap(), None);
    }
}
