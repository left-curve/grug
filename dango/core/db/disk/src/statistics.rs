use {
    crate::{
        CF_NAME_DEFAULT, CF_NAME_STATE_COMMITMENT, CF_NAME_STATE_STORAGE, CF_NAME_WASM_STORAGE,
        Data, cf_default, cf_state_commitment, cf_state_storage, cf_wasm_storage,
    },
    parking_lot::RwLock,
    rocksdb::{Options, properties::*, statistics::Histogram},
    std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    },
};

pub const ROCKSDB_STATISTICS: [(&str, &[(&PropName, &str)]); 10] = [
    // ============= BYTES =============
    // Memtable (RAM)
    (
        "rocksdb_memtable_bytes",
        &[
            (CUR_SIZE_ACTIVE_MEM_TABLE, "cur_size_active"),
            (CUR_SIZE_ALL_MEM_TABLES, "cur_size_all"),
            (SIZE_ALL_MEM_TABLES, "size_all"),
        ],
    ),
    // SST / on-disk space
    (
        "rocksdb_sst_bytes",
        &[
            (LIVE_SST_FILES_SIZE, "live_sst_files_size"),
            (TOTAL_SST_FILES_SIZE, "total_sst_files_size"),
            (ESTIMATE_LIVE_DATA_SIZE, "estimate_live_data_size"),
        ],
    ),
    // Compaction backlog (bytes to rewrite)
    (
        "rocksdb_compaction_bytes",
        &[(
            ESTIMATE_PENDING_COMPACTION_BYTES,
            "estimate_pending_compaction",
        )],
    ),
    // Block cache usage
    (
        "rocksdb_block_cache_bytes",
        &[
            (BLOCK_CACHE_CAPACITY, "capacity"),
            (BLOCK_CACHE_USAGE, "usage"),
            (BLOCK_CACHE_PINNED_USAGE, "pinned_usage"),
        ],
    ),
    // ============= COUNTS =============
    // Memtable entries & deletes
    (
        "rocksdb_memtable_count",
        &[
            (NUM_ENTRIES_ACTIVE_MEM_TABLE, "entries_active"),
            (NUM_ENTRIES_IMM_MEM_TABLES, "entries_imm"),
            (NUM_DELETES_ACTIVE_MEM_TABLE, "deletes_active"),
            (NUM_DELETES_IMM_MEM_TABLES, "deletes_imm"),
        ],
    ),
    // Memtable state (immutables, flushes, etc.)
    (
        "rocksdb_memtable_state_count",
        &[
            (NUM_IMMUTABLE_MEM_TABLE, "immutable"),
            (NUM_IMMUTABLE_MEM_TABLE_FLUSHED, "immutable_flushed"),
            (NUM_RUNNING_FLUSHES, "running_flushes"),
        ],
    ),
    // Active compactions
    (
        "rocksdb_compaction_count",
        &[(NUM_RUNNING_COMPACTIONS, "running_compactions")],
    ),
    // LSM structure info
    (
        "rocksdb_lsm_count",
        &[
            (NUM_LIVE_VERSIONS, "live_versions"),
            (CURRENT_SUPER_VERSION_NUMBER, "super_version_number"),
        ],
    ),
    // Background errors
    (
        "rocksdb_errors_count",
        &[(BACKGROUND_ERRORS, "background_errors")],
    ),
    // ============= FLAGS (0/1) =============
    (
        "rocksdb_flags",
        &[
            (COMPACTION_PENDING, "compaction_pending"),
            (MEM_TABLE_FLUSH_PENDING, "memtable_flush_pending"),
            (IS_WRITE_STOPPED, "is_write_stopped"),
            (IS_FILE_DELETIONS_ENABLED, "is_file_deletions_enabled"),
        ],
    ),
];

pub(crate) struct StatisticsWorker {
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl StatisticsWorker {
    pub fn run(opts: Options, inner: Arc<RwLock<Data>>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = thread::spawn(move || {
            // `interval` defines how often metrics should be emitted (every 5 seconds).
            // `poll_sleep` is a short sleep used inside the loop to periodically check
            // whether the worker has been asked to stop.
            //
            // We intentionally separate the long metrics interval from the short polling
            // sleep: if the worker only slept for the full `interval`, shutting it down
            // would block up to that duration (e.g. 5 seconds). By using a small
            // `poll_sleep`, the thread becomes responsive to shutdown requests and exits
            // within a few milliseconds, while still emitting metrics at the correct rate.
            let poll_sleep = Duration::from_millis(50);
            let interval = Duration::from_secs(5);
            let mut last_run = Instant::now();

            while !stop_clone.load(Ordering::SeqCst) {
                if last_run.elapsed() < interval {
                    thread::sleep(poll_sleep);
                    continue;
                }

                let guard = inner.read();

                for (cf_name, cf) in [
                    (CF_NAME_DEFAULT, cf_default(&guard.db)),
                    (CF_NAME_STATE_STORAGE, cf_state_storage(&guard.db)),
                    (CF_NAME_STATE_COMMITMENT, cf_state_commitment(&guard.db)),
                    (CF_NAME_WASM_STORAGE, cf_wasm_storage(&guard.db)),
                ] {
                    // ======== PROPERTIES (inner.db) ========

                    for (category, properties) in ROCKSDB_STATISTICS {
                        for (prop_name, label) in properties {
                            if let Ok(Some(v)) = guard.db.property_int_value_cf(cf, *prop_name) {
                                metrics::gauge!(category, "type" => *label, "cf" => cf_name)
                                    .set(v as f64);
                            }
                        }
                    }

                    // Number of SST files per LSM level (crucial for iterator performance)
                    for level in 0..7 {
                        let prop = num_files_at_level(level);
                        if let Ok(Some(v)) = guard.db.property_int_value_cf(cf, &prop) {
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

                // ======== STATISTICS (opts) ========

                // Iterator cost (p95 in microseconds)
                let h = opts.get_histogram_data(Histogram::DbSeek);
                metrics::gauge!("rocksdb_latency_micros", "type" => "iter_seek_p95").set(h.p95());

                last_run = Instant::now();
            }
        });

        Self {
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for StatisticsWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
