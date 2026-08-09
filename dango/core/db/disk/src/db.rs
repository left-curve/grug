#[cfg(feature = "tracing")]
use uuid::Uuid;
#[cfg(feature = "metrics")]
use {crate::statistics, dango_primitives::MetricsIterExt};
use {
    crate::{DbError, DbResult},
    dango_app::{CONTRACT_NAMESPACE, Commitment, Db, NAMESPACE_LEN},
    dango_primitives::{Addr, Batch, Buffer, Hash256, HashExt, Op, Order, Record, Storage},
    itertools::Itertools,
    parking_lot::{ArcRwLockReadGuard, RawRwLock, RwLock},
    rocksdb::{
        BlockBasedOptions, Cache, ColumnFamily, CompactionPri, DB, DBCompactionStyle, IteratorMode,
        Options, ReadOptions, SliceTransform, WriteBatch,
    },
    std::{collections::BTreeMap, marker::PhantomData, ops::Bound, path::Path, sync::Arc},
};

/// We use three column families (CFs) for storing data.
/// The default family is used for metadata. Currently the only metadata we have
/// is the latest version.
pub const CF_NAME_DEFAULT: &str = "default";

/// The state commitment (SC) family stores Merkle tree nodes, which hold hashed
/// key-value pair data. We use this CF for deriving the Merkle root hash for the
/// state (used in consensus) and generating Merkle proofs (used in light clients).
pub const CF_NAME_STATE_COMMITMENT: &str = "state_commitment";

/// The state storage (SS) family stores raw, prehash key-value pair data.
/// When performing normal read/write/remove/scan interactions, we use this CF.
pub const CF_NAME_STATE_STORAGE: &str = "state_storage";

pub const CF_NAME_WASM_STORAGE: &str = "wasm_storage";

/// Storage key for the latest version.
pub const LATEST_VERSION_KEY: &[u8] = b"latest_version";

#[cfg(feature = "metrics")]
pub const DISK_DB_LABEL: &str = "grug.db.disk.duration";

#[cfg(feature = "metrics")]
pub const ITERATOR_DB_LABEL: &str = "grug.db.disk.iterator.count";

#[cfg(feature = "metrics")]
pub const PRIORITY_DATA_LABEL: &str = "priority_data";

#[cfg(feature = "metrics")]
pub const WASM_STORAGE_LABEL: &str = "wasm";

#[cfg(feature = "metrics")]
pub const STATE_STORAGE_LABEL: &str = "state";

/// We assume all keys within the wasm range has the following format:
///
/// ```plain
/// b"wasm" | address | sub_key
/// ```
///
/// `address` is exactly 20 bytes. `sub_key` is 0 or more bytes.
///
/// As such, we can say that each `sub_key` is prefixed by exactly 24 bytes.
const WASM_PREFIX_LEN: usize = CONTRACT_NAMESPACE.len() + Addr::LENGTH; // = 4 + 20 = 24

/// The upper bound (exclusive) of the wasm range. Equals `increment_last_bytes(CONTRACT_NAMESPACE)`.
const CONTRACT_NAMESPACE_PLUS_ONE: &[u8] = b"wasn";

/// The base storage primitive.
///
/// Its main feature is the separation of state storage (SS) and state commitment
/// (SC). Specifically, SS stores _prehash_ key-value pairs (KV pairs), while SC
/// stores _hashed_ KV pairs in a Merkle tree data structure. SS is used for
/// normal read/write access to the state, while SC is used for deriving Merkle
/// root hashes for the state (used by nodes to reach consensus) and generate
/// Merkle proofs for data in the state (used by light clients).
///
/// The separation of SS and SC was first conceived by the Cosmos SDK core team
/// in ADR-040:
/// <https://github.com/cosmos/cosmos-sdk/blob/main/docs/architecture/adr-040-storage-and-smt-state-commitments.md>
/// and later refined in ADR-065:
/// <https://github.com/cosmos/cosmos-sdk/blob/main/docs/architecture/adr-065-store-v2.md>.
/// It was first pushed to production use by Sei, marketed as SeiDB:
/// <https://blog.sei.io/sei-db-the-numbers/>
///
/// Our design mostly resembles Sei's with the differences being that:
/// - we use a binary Jellyfish Merkle tree (JMT) instead of IAVL;
/// - we store JMT data in a RocksDB instance, instead of using memory map (mmap);
/// - we don't have asynchronous commit;
/// - we don't store snapshots or use a WAL to recover the latest state.
///
/// These differences are not because we don't agree with Sei's approach...
/// it's just because we're having here is sort of a quick hack and we don't
/// have time to look into those advanced features yet. We will keep experimenting
/// and maybe our implementation will converge with Sei's some time later.
pub struct DiskDb<T> {
    /// Data in the database.
    pub(crate) data: Arc<RwLock<Data>>,
    /// Data staged to, but not yet, be committed to the database.
    pending: Arc<RwLock<Option<PendingData>>>,
    /// The commitment scheme.
    _commitment: PhantomData<T>,
    /// Worker for emitting RocksDB statistics.
    #[cfg(feature = "metrics")]
    _statistics: Arc<statistics::StatisticsWorker>,
}

#[derive(Debug)]
pub(crate) struct Data {
    /// Represents an on-disk RocksDB instance.
    pub(crate) db: DB,
    /// Portion of the state storage that is critical to the chain's performance,
    /// loaded into memory. Reading these data doesn't need to touch the disk.
    priority_data: Option<PriorityData>,
}

#[derive(Debug)]
struct PriorityData {
    min: Vec<u8>, // inclusive
    max: Vec<u8>, // exclusive
    records: BTreeMap<Vec<u8>, Vec<u8>>,
}

#[derive(Debug)]
struct PendingData {
    version: u64,
    state_commitment: Batch,
    state_storage: Batch,
}

impl<T> DiskDb<T> {
    /// Create a DiskDb instance by opening a physical RocksDB instance.
    pub fn open<P>(data_dir: P) -> DbResult<Self>
    where
        P: AsRef<Path>,
    {
        Self::open_with_priority(data_dir, None::<(&[u8], &[u8])>)
    }

    /// Create a DiskDb instance by opening a physical RocksDB instance,
    /// optionally with a priority range. Records within the range will be
    /// loaded into memory for better performance. The range's lower bound is
    /// inclusive, the upper bound is exclusive.
    pub fn open_with_priority<P, B>(data_dir: P, priority_range: Option<(B, B)>) -> DbResult<Self>
    where
        P: AsRef<Path>,
        B: AsRef<[u8]>,
    {
        let opts = new_db_options();
        let cf_opts = new_state_cf_options();
        let wasm_cf_opts = new_wasm_cf_options(cf_opts.clone());
        let storage_cf_opts = new_storage_cf_options(cf_opts.clone());
        let db = DB::open_cf_with_opts(
            &opts,
            data_dir,
            [
                (CF_NAME_DEFAULT, Options::default()),
                (CF_NAME_STATE_STORAGE, storage_cf_opts),
                (CF_NAME_STATE_COMMITMENT, cf_opts),
                (CF_NAME_WASM_STORAGE, wasm_cf_opts),
            ],
        )?;

        // If `priority_range` is specified, load the data in that range into memory.
        let priority_data = priority_range.map(|(min, max)| {
            #[cfg(feature = "tracing")]
            let mut size = 0;

            let records = create_rocksdb_storage_iter(
                &db,
                Some(min.as_ref()),
                Some(max.as_ref()),
                Order::Ascending,
                #[cfg(feature = "metrics")]
                "priority_data/init",
            )
            .inspect(|(_k, _v)| {
                #[cfg(feature = "tracing")]
                {
                    size += _k.len() + _v.len();
                }
            })
            .collect::<BTreeMap<_, _>>();

            #[cfg(feature = "tracing")]
            {
                tracing::info!(num_records = records.len(), size, "Loaded priority data");
            }

            PriorityData {
                min: min.as_ref().to_vec(),
                max: max.as_ref().to_vec(),
                records,
            }
        });

        let data = Arc::new(RwLock::new(Data { db, priority_data }));

        #[cfg(feature = "metrics")]
        let handle = statistics::StatisticsWorker::run(opts, Arc::clone(&data));

        Ok(Self {
            data,
            pending: Arc::new(RwLock::new(None)),
            _commitment: PhantomData,
            #[cfg(feature = "metrics")]
            _statistics: Arc::new(handle),
        })
    }
}

impl<T> Clone for DiskDb<T> {
    fn clone(&self) -> Self {
        Self {
            data: Arc::clone(&self.data),
            pending: Arc::clone(&self.pending),
            _commitment: PhantomData,
            #[cfg(feature = "metrics")]
            _statistics: Arc::clone(&self._statistics),
        }
    }
}

impl<T> Db for DiskDb<T>
where
    T: Commitment,
{
    type Error = DbError;
    type Proof = T::Proof;
    type StateCommitment = StateCommitment;
    type StateStorage = StateStorage;

    fn state_commitment(&self) -> StateCommitment {
        StateCommitment::new(&self.data)
    }

    fn state_storage_with_comment(
        &self,
        version: Option<u64>,
        comment: &'static str,
    ) -> DbResult<StateStorage> {
        // If version is unspecified, use the latest version. Otherwise, make
        // sure it's no newer than the latest version.
        if let Some(version) = version {
            // Read the latest version.
            // If it doesn't exist, this means not even a single batch has been
            // written yet (e.g. during `InitChain`). In this case just use zero.
            let latest_version = self.latest_version().unwrap_or(0);
            if version != latest_version {
                return Err(DbError::incorrect_version(version, latest_version));
            }
        }

        Ok(StateStorage::new(&self.data, comment))
    }

    fn latest_version(&self) -> Option<u64> {
        #[cfg(feature = "tracing")]
        let uuid = Uuid::new_v4().to_string();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(
                kind = "read",
                uuid,
                comment = "latest_version",
                "Locking data"
            );
        }

        let latest_version = {
            let data = self.data.read();

            #[cfg(feature = "tracing")]
            {
                tracing::debug!(
                    kind = "read",
                    uuid,
                    comment = "latest_version",
                    "Locked data"
                );
            }

            let bytes = data
                .db
                .get_cf(&cf_default(&data.db), LATEST_VERSION_KEY)
                .unwrap_or_else(|err| {
                    panic!("failed to read latest version from default column family: {err}");
                })?
                .try_into()
                .unwrap_or_else(|bytes: Vec<u8>| {
                    panic!("latest version is of incorrect length: {}", bytes.len());
                });

            Some(u64::from_le_bytes(bytes))
        };

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(
                kind = "read",
                uuid,
                comment = "latest_version",
                "Unlocked data"
            );
        }

        latest_version
    }

    fn oldest_version(&self) -> Option<u64> {
        // This database isn't archival, meaning it only keeps the most recent
        // version, so the oldest available version is the same as the latest version.
        self.latest_version()
    }

    fn root_hash(&self, version: Option<u64>) -> DbResult<Option<Hash256>> {
        let version = version.unwrap_or_else(|| self.latest_version().unwrap_or(0));
        Ok(T::root_hash(&self.state_commitment(), version)?)
    }

    fn prove(&self, key: &[u8], version: Option<u64>) -> DbResult<Self::Proof> {
        let version = version.unwrap_or_else(|| self.latest_version().unwrap_or(0));
        Ok(T::prove(&self.state_commitment(), key.sha2_256(), version)?)
    }

    fn flush_but_not_commit(&self, batch: Batch) -> DbResult<(u64, Option<Hash256>)> {
        #[cfg(feature = "metrics")]
        let duration = std::time::Instant::now();

        // A write batch must not already exist. If it does, it means a batch
        // has been flushed, but not committed, then a next batch is flusehd,
        // which indicates some error in the ABCI app's logic.
        if self.pending.read().is_some() {
            return Err(DbError::pending_data_already_set());
        }

        // We assume Dango App has the following behavior: for each key that is
        // prefixed with `b"wasm"`, it must be a wasm key. See the comments of
        // `WASM_PREFIX_LEN` and the `is_wasm_key` function for explanation.
        if let Some(key) = batch
            .keys()
            .find(|k| k.starts_with(CONTRACT_NAMESPACE) && !is_wasm_key(k))
        {
            return Err(DbError::not_wasm_key(key.clone()));
        }

        let (old_version, new_version) = match self.latest_version() {
            // An old version exist.
            // Set the new version to be the old version plus one
            Some(v) => (v, v + 1),
            // The old version doesn't exist. This means not a first batch has
            // been flushed yet. In this case, we set the new version to be zero.
            // This is necessary to ensure that DB version always matches the
            // block height.
            None => (0, 0),
        };

        // Commit hashed KVs to state commitment.
        // The DB writes here are kept in the in-memory `PendingData`.
        let mut buffer = Buffer::new(
            self.state_commitment(),
            None,
            "disk_db/state_commitment/flush_but_not_commit",
        );

        let root_hash = T::apply(&mut buffer, old_version, new_version, &batch)?;
        let (_, pending) = buffer.disassemble();

        *(self.pending.write()) = Some(PendingData {
            version: new_version,
            state_commitment: pending,
            state_storage: batch,
        });

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(DISK_DB_LABEL, "operation" => "flush_but_not_commit")
                .record(duration.elapsed().as_secs_f64());
        }

        Ok((new_version, root_hash))
    }

    fn commit(&self) -> DbResult<u64> {
        #[cfg(feature = "metrics")]
        let duration = std::time::Instant::now();

        let pending = self
            .pending
            .write()
            .take()
            .ok_or(DbError::pending_data_not_set())?;

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(kind = "write", comment = "commit", "Locking data");
        }

        let mut data = self.data.write();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(kind = "write", comment = "commit", "Locked data");
        }

        // If priority data exists, apply the change set to it.
        if let Some(priority) = &mut data.priority_data {
            for (k, op) in pending.state_storage.range::<[u8], _>((
                Bound::Included(priority.min.as_slice()),
                Bound::Excluded(priority.max.as_slice()),
            )) {
                if let Op::Insert(v) = op {
                    priority.records.insert(k.clone(), v.clone());
                } else {
                    priority.records.remove(k);
                }
            }
        }

        // Now, prepare the write batch that will be written to RocksDB.
        let mut batch = WriteBatch::default();

        // Set the new version (note: use little endian)
        let cf = cf_default(&data.db);
        batch.put_cf(&cf, LATEST_VERSION_KEY, pending.version.to_le_bytes());

        // Writes in state commitment
        let cf = cf_state_commitment(&data.db);
        for (key, op) in pending.state_commitment {
            if let Op::Insert(value) = op {
                batch.put_cf(&cf, key, value);
            } else {
                batch.delete_cf(&cf, key);
            }
        }

        // Writes in state storage
        let cf_state = cf_state_storage(&data.db);
        let cf_wasm = cf_wasm_storage(&data.db);
        for (key, op) in pending.state_storage {
            let cf = if is_wasm_key(&key) {
                cf_wasm
            } else {
                cf_state
            };

            if let Op::Insert(value) = op {
                batch.put_cf(&cf, key, value);
            } else {
                batch.delete_cf(&cf, key);
            }
        }

        data.db.write(batch)?;

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(kind = "write", comment = "commit", "Unlocked data");
        }

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(DISK_DB_LABEL, "operation" => "commit")
                .record(duration.elapsed().as_secs_f64());
        }

        Ok(pending.version)
    }

    fn prune(&self, up_to_version: u64) -> DbResult<()> {
        #[cfg(feature = "tracing")]
        {
            tracing::info!(up_to_version, "Pruning database");
        }

        #[cfg(feature = "metrics")]
        let duration = std::time::Instant::now();

        // Prune state commitment.
        let mut buffer = Buffer::new(
            self.state_commitment(),
            None,
            "disk_db/state_commitment/prune",
        );

        T::prune(&mut buffer, up_to_version)?;

        let (_, pending) = buffer.disassemble();
        let mut batch = WriteBatch::default();

        {
            let data = self.data.write();

            let cf = cf_state_commitment(&data.db);
            for (key, op) in pending {
                if let Op::Insert(value) = op {
                    batch.put_cf(&cf, key, value);
                } else {
                    batch.delete_cf(&cf, key);
                }
            }

            data.db.write(batch)?;
        }

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(DISK_DB_LABEL, "operation" => "prune")
                .record(duration.elapsed().as_secs_f64());
        }

        Ok(())
    }
}

// ----------------------------- state commitment ------------------------------

#[derive(Debug)]
pub struct StateCommitment {
    guard: Arc<ArcRwLockReadGuard<RawRwLock, Data>>,
    #[cfg(feature = "tracing")]
    uuid: String,
}

impl StateCommitment {
    fn new(data: &Arc<RwLock<Data>>) -> Self {
        #[cfg(feature = "tracing")]
        let uuid = Uuid::new_v4().to_string();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(
                kind = "read",
                comment = "state_commitment",
                uuid,
                "Locking data"
            );
        }

        let guard = data.read_arc();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(
                kind = "read",
                comment = "state_commitment",
                uuid,
                "Locked data"
            );
        }

        Self {
            guard: Arc::new(guard),
            #[cfg(feature = "tracing")]
            uuid,
        }
    }
}

// When cloning, instead of acquiring a new read-lock, we create a new shared
// reference to the same read-lock. The idea is that there should be only one
// lock through the entire lifetime of this `StateCommitment`.
impl Clone for StateCommitment {
    fn clone(&self) -> Self {
        #[cfg(feature = "tracing")]
        let uuid = Uuid::new_v4().to_string();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(
                kind = "read",
                comment = "state_commitment",
                strong_count = Arc::strong_count(&self.guard),
                from = self.uuid,
                to = uuid,
                "Lock cloned"
            );
        }

        Self {
            guard: Arc::clone(&self.guard),
            #[cfg(feature = "tracing")]
            uuid,
        }
    }
}

#[cfg(feature = "tracing")]
impl Drop for StateCommitment {
    fn drop(&mut self) {
        tracing::debug!(
            kind = "read",
            comment = "state_commitment",
            strong_count = Arc::strong_count(&self.guard),
            uuid = self.uuid,
            "Lock clone dropped"
        );
    }
}

impl Storage for StateCommitment {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        let cf = cf_state_commitment(&self.guard.db);
        self.guard.db.get_cf(&cf, key).unwrap_or_else(|err| {
            panic!("failed to read from state commitment: {err}");
        })
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let opts = new_read_options(min, max);
        let mode = into_iterator_mode(order);
        let iter = self
            .guard
            .db
            .iterator_cf_opt(&cf_state_commitment(&self.guard.db), opts, mode)
            .map(|item| {
                let (k, v) = item.unwrap_or_else(|err| {
                    panic!("failed to iterate in state commitment: {err}");
                });
                (k.to_vec(), v.to_vec())
            });
        Box::new(iter)
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let opts = new_read_options(min, max);
        let mode = into_iterator_mode(order);
        let iter = self
            .guard
            .db
            .iterator_cf_opt(&cf_state_commitment(&self.guard.db), opts, mode)
            .map(|item| {
                let (k, _) = item.unwrap_or_else(|err| {
                    panic!("failed to iterate in state commitment: {err}");
                });
                k.to_vec()
            });
        Box::new(iter)
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let opts = new_read_options(min, max);
        let mode = into_iterator_mode(order);
        let iter = self
            .guard
            .db
            .iterator_cf_opt(&cf_state_commitment(&self.guard.db), opts, mode)
            .map(|item| {
                let (_, v) = item.unwrap_or_else(|err| {
                    panic!("failed to iterate in state commitment: {err}");
                });
                v.to_vec()
            });
        Box::new(iter)
    }

    fn write(&mut self, _key: &[u8], _value: &[u8]) {
        unreachable!("write function called on read-only storage");
    }

    fn remove(&mut self, _key: &[u8]) {
        unreachable!("write function called on read-only storage");
    }

    fn remove_range(&mut self, _min: Option<&[u8]>, _max: Option<&[u8]>) {
        unreachable!("write function called on read-only storage");
    }
}

// ------------------------------- state storage -------------------------------

#[derive(Debug)]
pub struct StateStorage {
    guard: Arc<ArcRwLockReadGuard<RawRwLock, Data>>,
    comment: &'static str,
    #[cfg(feature = "tracing")]
    uuid: String,
}

impl StateStorage {
    fn new(data: &Arc<RwLock<Data>>, comment: &'static str) -> Self {
        #[cfg(feature = "tracing")]
        let uuid = Uuid::new_v4().to_string();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(kind = "read", uuid, comment, "Locking data");
        }

        let guard = data.read_arc();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(kind = "read", uuid, comment, "Locked data");
        }

        Self {
            guard: Arc::new(guard),
            comment,
            #[cfg(feature = "tracing")]
            uuid,
        }
    }
}

// When cloning, instead of acquiring a new read-lock, we create a new shared
// reference to the same read-lock. The idea is that there should be only one
// lock through the entire lifetime of this `StateStorage`.
impl Clone for StateStorage {
    fn clone(&self) -> Self {
        #[cfg(feature = "tracing")]
        let uuid = Uuid::new_v4().to_string();

        #[cfg(feature = "tracing")]
        {
            tracing::debug!(
                kind = "read",
                comment = self.comment,
                strong_count = Arc::strong_count(&self.guard),
                from = self.uuid,
                to = uuid,
                "Lock cloned"
            );
        }

        Self {
            guard: Arc::clone(&self.guard),
            comment: self.comment,
            #[cfg(feature = "tracing")]
            uuid,
        }
    }
}

#[cfg(feature = "tracing")]
impl Drop for StateStorage {
    fn drop(&mut self) {
        tracing::debug!(
            kind = "read",
            comment = self.comment,
            strong_count = Arc::strong_count(&self.guard),
            uuid = self.uuid,
            "Lock clone dropped"
        );
    }
}

impl Storage for StateStorage {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        #[cfg(feature = "metrics")]
        let duration = std::time::Instant::now();

        // If the key falls in the priority data range, read the value from
        // priority data, without accessing the disk.
        // Note: `min` is inclusive, while `max` is exclusive.
        if let Some(data) = &self.guard.priority_data
            && data.min.as_slice() <= key
            && key < data.max.as_slice()
        {
            let value = data.records.get(key).cloned();

            #[cfg(feature = "metrics")]
            {
                metrics::histogram!(
                    DISK_DB_LABEL,
                    "operation" => "read",
                    "comment" => self.comment,
                    "source" => PRIORITY_DATA_LABEL
                )
                .record(duration.elapsed().as_secs_f64());
            }

            return value;
        }

        let cf = if is_wasm_key(key) {
            cf_wasm_storage(&self.guard.db)
        } else {
            cf_state_storage(&self.guard.db)
        };

        let opts = new_read_options(None, None);
        let value = self
            .guard
            .db
            .get_cf_opt(&cf, key, &opts)
            .unwrap_or_else(|err| {
                panic!("failed to read from state storage: {err}");
            });

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(
                DISK_DB_LABEL,
                "operation" => "read",
                "comment" => self.comment,
                "source" => if is_wasm_key(key) {
                    WASM_STORAGE_LABEL
                } else {
                    STATE_STORAGE_LABEL
                }
            )
            .record(duration.elapsed().as_secs_f64());
        }

        value
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        // If priority data exists, and the iterator range completely falls
        // within the priority range, then create a priority iterator.
        // Note: `min` and `data.min` are both inclusive; `max` and `data.max`
        // are both exclusive.
        // TODO: the nested `if` statements can be simplified with rust edition 2024
        if let (Some(data), Some(min), Some(max)) = (&self.guard.priority_data, min, max)
            && data.min.as_slice() <= min
            && max <= data.max.as_slice()
        {
            #[cfg(feature = "metrics")]
            let duration = std::time::Instant::now();

            let iter = data.records.scan(Some(min), Some(max), order);

            #[cfg(feature = "metrics")]
            {
                let iter = iter.with_metrics(
                    DISK_DB_LABEL,
                    [
                        ("operation", "next"),
                        ("comment", self.comment),
                        ("source", PRIORITY_DATA_LABEL),
                    ],
                );

                metrics::histogram!(
                    DISK_DB_LABEL,
                    "operation" => "scan",
                    "comment" => self.comment,
                    "source" => PRIORITY_DATA_LABEL
                )
                .record(duration.elapsed().as_secs_f64());

                return Box::new(iter);
            }

            #[cfg(not(feature = "metrics"))]
            return iter;
        }

        let iter = create_rocksdb_storage_iter(
            &self.guard.db,
            min,
            max,
            order,
            #[cfg(feature = "metrics")]
            self.comment,
        );

        Box::new(iter)
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let iter = self.scan(min, max, order).map(|(k, _)| k);
        Box::new(iter)
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let iter = self.scan(min, max, order).map(|(_, v)| v);
        Box::new(iter)
    }

    fn write(&mut self, _key: &[u8], _value: &[u8]) {
        unreachable!("write function called on read-only storage");
    }

    fn remove(&mut self, _key: &[u8]) {
        unreachable!("write function called on read-only storage");
    }

    fn remove_range(&mut self, _min: Option<&[u8]>, _max: Option<&[u8]>) {
        unreachable!("write function called on read-only storage");
    }
}

// --------------------------------- iteration ---------------------------------

fn create_rocksdb_storage_iter<'a>(
    db: &'a DB,
    min: Option<&[u8]>,
    max: Option<&[u8]>,
    order: Order,
    #[cfg(feature = "metrics")] comment: &'static str,
) -> Box<dyn Iterator<Item = Record> + 'a> {
    match (min, max) {
        // If the range falls within the wasm range, iterate only the wasm CF.
        (Some(min), Some(max))
            if CONTRACT_NAMESPACE <= min && max <= CONTRACT_NAMESPACE_PLUS_ONE =>
        {
            create_wasm_iter(
                db,
                Some(min),
                Some(max),
                order,
                #[cfg(feature = "metrics")]
                comment,
            )
        },
        // If the range is completely bigger than the wasm range, iterate only
        // the storage CF.
        (Some(min), max) if CONTRACT_NAMESPACE_PLUS_ONE <= min => create_state_iter(
            db,
            Some(min),
            max,
            order,
            #[cfg(feature = "metrics")]
            comment,
        ),
        // If the range is completely smaller than the wasm range, iterate only
        // the storage CF.
        (min, Some(max)) if max <= CONTRACT_NAMESPACE => create_state_iter(
            db,
            min,
            Some(max),
            order,
            #[cfg(feature = "metrics")]
            comment,
        ),
        // In all other cases, iterate BOTH the wasm and storage CFs.
        _ => create_merged_iter(
            db,
            min,
            max,
            order,
            #[cfg(feature = "metrics")]
            comment,
        ),
    }
}

fn create_wasm_iter<'a>(
    db: &'a DB,
    min: Option<&[u8]>,
    max: Option<&[u8]>,
    order: Order,
    #[cfg(feature = "metrics")] comment: &'static str,
) -> Box<dyn Iterator<Item = Record> + 'a> {
    #[cfg(feature = "metrics")]
    let duration = std::time::Instant::now();

    let opts = new_wasm_read_options(new_read_options(min, max), min, max);
    let mode = into_iterator_mode(order);

    let iter = db
        .iterator_cf_opt(&cf_wasm_storage(db), opts, mode)
        .map(|item| {
            let (k, v) = item.unwrap_or_else(|err| {
                panic!("failed to iterate in state storage: {err}");
            });
            (k.to_vec(), v.to_vec())
        });

    #[cfg(feature = "metrics")]
    let iter = iter.with_metrics(
        DISK_DB_LABEL,
        [
            ("operation", "next"),
            ("comment", comment),
            ("source", WASM_STORAGE_LABEL),
        ],
    );

    #[cfg(feature = "metrics")]
    {
        metrics::histogram!(
            DISK_DB_LABEL,
            "operation" => "scan",
            "comment" => comment,
            "source" => WASM_STORAGE_LABEL
        )
        .record(duration.elapsed().as_secs_f64());
    };

    Box::new(iter)
}

fn create_state_iter<'a>(
    db: &'a DB,
    min: Option<&[u8]>,
    max: Option<&[u8]>,
    order: Order,
    #[cfg(feature = "metrics")] comment: &'static str,
) -> Box<dyn Iterator<Item = Record> + 'a> {
    #[cfg(feature = "metrics")]
    let duration = std::time::Instant::now();

    let opts = new_storage_read_options(new_read_options(min, max), min, max);
    let mode = into_iterator_mode(order);

    let iter = db
        .iterator_cf_opt(&cf_state_storage(db), opts, mode)
        .map(|item| {
            let (k, v) = item.unwrap_or_else(|err| {
                panic!("failed to iterate in state storage: {err}");
            });
            (k.to_vec(), v.to_vec())
        });

    #[cfg(feature = "metrics")]
    let iter = iter.with_metrics(
        DISK_DB_LABEL,
        [
            ("operation", "next"),
            ("comment", comment),
            ("source", STATE_STORAGE_LABEL),
        ],
    );

    #[cfg(feature = "metrics")]
    {
        metrics::histogram!(
            DISK_DB_LABEL,
            "operation" => "scan",
            "comment" => comment,
            "source" => STATE_STORAGE_LABEL
        )
        .record(duration.elapsed().as_secs_f64());
    };

    Box::new(iter)
}

fn create_merged_iter<'a>(
    db: &'a DB,
    min: Option<&[u8]>,
    max: Option<&[u8]>,
    order: Order,
    #[cfg(feature = "metrics")] comment: &'static str,
) -> Box<dyn Iterator<Item = Record> + 'a> {
    let wasm_iter = create_wasm_iter(
        db,
        min,
        max,
        order,
        #[cfg(feature = "metrics")]
        comment,
    );
    let state_iter = create_state_iter(
        db,
        min,
        max,
        order,
        #[cfg(feature = "metrics")]
        comment,
    );

    if let Order::Ascending = order {
        Box::new(wasm_iter.merge_by(state_iter, |a, b| a.0 < b.0))
    } else {
        Box::new(wasm_iter.merge_by(state_iter, |a, b| a.0 > b.0))
    }
}

pub fn is_wasm_key(key: &[u8]) -> bool {
    key.starts_with(CONTRACT_NAMESPACE) && key.len() >= WASM_PREFIX_LEN
}

// ---------------------------------- helpers ----------------------------------

#[inline]
fn into_iterator_mode(order: Order) -> IteratorMode<'static> {
    match order {
        Order::Ascending => IteratorMode::Start,
        Order::Descending => IteratorMode::End,
    }
}

// Reference for RocksDB tuning:
// https://github.com/sei-protocol/sei-db/blob/main/ss/rocksdb/opts.go#L29-L65
// https://github.com/turbofish-org/merk/blob/develop/src/merk/mod.rs#L84-L102
pub fn new_db_options() -> Options {
    let mut opts = Options::default();

    // Create the DB and CFs if they do not exist.
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    // Increase background parallelism: flushes and compactions run in more threads.
    // Helps maintain healthy LSM structure without blocking foreground operations.
    opts.increase_parallelism(num_cpus::get() as i32);

    // Give RocksDB a memory budget to tune level-style compaction.
    // This optimizes compaction strategy and usually reduces write amplification.
    opts.optimize_level_style_compaction(256 * 1024 * 1024);

    // Remove limit on open file descriptors, avoiding overhead of
    // frequently opening/closing SST files.
    opts.set_max_open_files(-1); // -1 = unlimited

    opts
}

/// Create a tuned default `Options` for a colume family.
pub fn new_state_cf_options() -> Options {
    let mut opts = Options::default();

    opts.set_max_write_buffer_number(2); // Default is 2
    opts.set_min_write_buffer_number_to_merge(1); // Default is 1

    // ---- L0 ----
    opts.set_level_zero_file_num_compaction_trigger(2); // Default is 4
    opts.set_level_zero_slowdown_writes_trigger(8); // Default is 20
    opts.set_level_zero_stop_writes_trigger(16); // Default is 24

    opts.set_target_file_size_base(8 * 1024 * 1024); // ~8MB per SST L1 (Default is 64MB)
    opts.set_max_bytes_for_level_base(64 * 1024 * 1024); // ~64MB per L1 (Default is 256MB)

    opts.set_max_open_files(-1); // Default is -1

    // ---- Compaction ----
    opts.set_compaction_style(DBCompactionStyle::Level); // Default is DBCompactionStyle::Level
    opts.set_compaction_pri(CompactionPri::ByCompensatedSize); // Default is CompactionPri::ByCompensatedSize

    // ---- Block-based table ----
    let mut block_opts = BlockBasedOptions::default();
    block_opts.set_block_cache(&Cache::new_lru_cache(256 * 1024 * 1024)); // Default is 8MB
    block_opts.set_bloom_filter(10.0, true);
    block_opts.set_cache_index_and_filter_blocks(true);
    block_opts.set_pin_l0_filter_and_index_blocks_in_cache(true); // Default is false

    opts.set_block_based_table_factory(&block_opts);
    opts
}

/// Create an `Options` specifically for the Wasm column family, given an existing
/// base `Options`.
pub fn new_wasm_cf_options(mut opts: Options) -> Options {
    // ---- Memtable ----
    //
    // Wasm contract storage tends to generate far fewer tombstones than the
    // chain-level state CF, and also have a strong prefix locality (fixed 24-byte prefix).
    //
    // A 16MB memtable gives:
    // - fewer flushes during periods of high contract activity,
    // - better write throughput,
    // - minimal impact on iterator performance, since wasm keys have a
    //   strong prefix locality (fixed 24-byte prefix).
    //
    // Adjust later if specific contracts generate large amounts of deletes.
    opts.set_write_buffer_size(16 * 1024 * 1024); // Default is 64MB
    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(WASM_PREFIX_LEN));
    opts
}

pub fn new_storage_cf_options(mut opts: Options) -> Options {
    // ---- Memtable ----
    // The state-storage CF is extremely delete-heavy (cronjobs), producing a
    // very large number of tombstones. A small memtable (2MB) keeps the number
    // of live entries + tombstones low at any moment, which greatly reduces
    // iterator construction time. With a 2MB memtable we flush frequently,
    // minimizing the worst-case "ramp-up" in iterator latency.
    opts.set_write_buffer_size(2 * 1024 * 1024); // Default is 64MB
    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(NAMESPACE_LEN));
    opts
}

/// Create a tuned default `ReadOptions`.
pub fn new_read_options(
    iterate_lower_bound: Option<&[u8]>,
    iterate_upper_bound: Option<&[u8]>,
) -> ReadOptions {
    let mut opts = ReadOptions::default();
    if let Some(bound) = iterate_lower_bound {
        opts.set_iterate_lower_bound(bound);
    }
    if let Some(bound) = iterate_upper_bound {
        opts.set_iterate_upper_bound(bound);
    }

    // Enable readahead for sequential scans. This reduces the number of
    // system calls and page faults when reading large ranges.
    opts.set_readahead_size(1000 * 1024);

    opts
}

/// Create a `ReadOptions` specifically for iteration in the Wasm column family,
/// given an existing `ReadOptions`.
pub fn new_wasm_read_options(
    mut opts: ReadOptions,
    min: Option<&[u8]>,
    max: Option<&[u8]>,
) -> ReadOptions {
    // Enable prefix mode only if `min` & `max` share the exact same "wasm" + addr prefix.
    if let (Some(min), Some(max)) = (min, max)
        && min.len() >= WASM_PREFIX_LEN
        && max.len() >= WASM_PREFIX_LEN
        && min[..WASM_PREFIX_LEN] == max[..WASM_PREFIX_LEN]
    {
        opts.set_prefix_same_as_start(true);
    }

    opts
}

pub fn new_storage_read_options(
    mut opts: ReadOptions,
    min: Option<&[u8]>,
    max: Option<&[u8]>,
) -> ReadOptions {
    if let (Some(min), Some(max)) = (min, max)
        && min.len() >= NAMESPACE_LEN
        && max.len() >= NAMESPACE_LEN
        && min[..NAMESPACE_LEN] == max[..NAMESPACE_LEN]
    {
        opts.set_prefix_same_as_start(true);
    }
    opts
}

pub fn cf_default(db: &DB) -> &ColumnFamily {
    db.cf_handle(CF_NAME_DEFAULT).unwrap_or_else(|| {
        panic!("failed to find default column family");
    })
}

pub fn cf_state_storage(db: &DB) -> &ColumnFamily {
    db.cf_handle(CF_NAME_STATE_STORAGE).unwrap_or_else(|| {
        panic!("failed to find state storage column family");
    })
}

pub fn cf_wasm_storage(db: &DB) -> &ColumnFamily {
    db.cf_handle(CF_NAME_WASM_STORAGE).unwrap_or_else(|| {
        panic!("failed to find wasm storage column family");
    })
}

pub fn cf_state_commitment(db: &DB) -> &ColumnFamily {
    db.cf_handle(CF_NAME_STATE_COMMITMENT).unwrap_or_else(|| {
        panic!("failed to find state commitment column family");
    })
}

// ------------------------ tests using JMT commitment -------------------------

#[cfg(test)]
mod tests_jmt {
    use {
        crate::DiskDb,
        dango_app::Db,
        dango_jmt::{MerkleTree, verify_proof},
        dango_primitives::{
            Batch, Hash256, HashExt, MembershipProof, NonMembershipProof, Op, Order, Proof,
            ProofNode, Storage,
        },
        dango_temp_rocksdb::TempDataDir,
        hex_literal::hex,
    };

    // Using the same test case as in our rust-rocksdb fork:
    // https://github.com/left-curve/rust-rocksdb/blob/v0.21.0-cw/tests/test_timestamp.rs#L150
    //
    // hash(donald)  = 01000001...
    // hash(jake)    = 11001101...
    // hash(joe)     = 01111000...
    // hash(larry)   = 00001101...
    // hash(pumpkin) = 11111111...

    // The tree at version 1 should look like:
    //
    //           root
    //         ┌──┴──┐
    //         0     jake
    //      ┌──┴──┐
    //    larry   01
    //         ┌──┴──┐
    //      donald  joe
    //
    // hash_00
    // = hash(01 | hash("larry") | hash("engineer"))
    // = hash(01 | 0d098b1c0162939e05719f059f0f844ed989472e9e6a53283a00fe92127ac27f | 7826b958b79c70626801b880405eb5111557dadceb2fee2b1ed69a18eed0c6dc)
    // = 01d2b46c3dd0180a5e8236137b4ada8ae6c9ca7c8799ecf7932d1320c9dfbf3b
    //
    // hash_010
    // = hash(01 | hash("donald") | hash("trump"))
    // = hash(01 | 4138cfbc5d36f31e8ae09ef4044bb88c0c9c6f289a6a1c27b335a99d1d8dc86f | a60a52382d7077712def2a69eda3ba309b19598944aa459ce418ae53b7fb5d58)
    // = 8fb3cdb9c15244dc8b7f701bb08640389dcde92a3b85277348ca1ec839d2a575
    //
    // hash_011
    // = hash(01 | hash("joe") | hash("biden"))
    // = hash(01 | 78675cc176081372c43abab3ea9fb70c74381eb02dc6e93fb6d44d161da6eeb3 | 0631a609edb7c79f3a051b935ddb0927818ebd03964a4d18f316d2dadf216894)
    // = hash(3b640fe6cffebfa7c2ba388b66aa3a4978c2221799ef9316e059eed2e656511a)
    //
    // hash_01
    // = hash(00 | 8fb3cdb9c15244dc8b7f701bb08640389dcde92a3b85277348ca1ec839d2a575 | 3b640fe6cffebfa7c2ba388b66aa3a4978c2221799ef9316e059eed2e656511a)
    // = 248f2dfa7cd94e3856e5a6978e500e6d9528837cd0c64187b937455f8d865baf
    //
    // hash_0
    // = hash(00 | 01d2b46c3dd0180a5e8236137b4ada8ae6c9ca7c8799ecf7932d1320c9dfbf3b | 248f2dfa7cd94e3856e5a6978e500e6d9528837cd0c64187b937455f8d865baf)
    // = 4d28a7511b5df59d1cdab1ace2314ba10f4637d0b51cac24ad0dbf199f7333ad
    //
    // hash_1
    // = hash(01 | hash("jake") | hash("shepherd"))
    // = hash(01 | cdf30c6b345276278bedc7bcedd9d5582f5b8e0c1dd858f46ef4ea231f92731d | def3735d7a0d2696775d6d72f379e4536c4d9e3cd6367f27a0bcb7f40d4558fb)
    // = 8358fe5d68c2d969c72b67ccffef68e2bf3b2edb200c0a7731e9bf131be11394
    //
    // root_hash
    // = hash(00 | 4d28a7511b5df59d1cdab1ace2314ba10f4637d0b51cac24ad0dbf199f7333ad | 8358fe5d68c2d969c72b67ccffef68e2bf3b2edb200c0a7731e9bf131be11394)
    // = 1712a8d4c9896a8cadb4e13592bd9e2713a16d0bf5572a8bf540eb568cb30b64
    mod v0 {
        use super::*;

        pub const ROOT_HASH: Hash256 = Hash256::from_inner(hex!(
            "1712a8d4c9896a8cadb4e13592bd9e2713a16d0bf5572a8bf540eb568cb30b64"
        ));
        pub const HASH_0: Hash256 = Hash256::from_inner(hex!(
            "4d28a7511b5df59d1cdab1ace2314ba10f4637d0b51cac24ad0dbf199f7333ad"
        ));
        pub const HASH_00: Hash256 = Hash256::from_inner(hex!(
            "01d2b46c3dd0180a5e8236137b4ada8ae6c9ca7c8799ecf7932d1320c9dfbf3b"
        ));
        pub const HASH_01: Hash256 = Hash256::from_inner(hex!(
            "248f2dfa7cd94e3856e5a6978e500e6d9528837cd0c64187b937455f8d865baf"
        ));
        pub const HASH_010: Hash256 = Hash256::from_inner(hex!(
            "8fb3cdb9c15244dc8b7f701bb08640389dcde92a3b85277348ca1ec839d2a575"
        ));
        pub const HASH_011: Hash256 = Hash256::from_inner(hex!(
            "3b640fe6cffebfa7c2ba388b66aa3a4978c2221799ef9316e059eed2e656511a"
        ));
        pub const HASH_1: Hash256 = Hash256::from_inner(hex!(
            "8358fe5d68c2d969c72b67ccffef68e2bf3b2edb200c0a7731e9bf131be11394"
        ));
    }

    // The tree at version 2 should look like:
    //
    //            root
    //         ┌───┴───┐
    //         0       1
    //      ┌──┴──┐    └──┐
    //   larry  donald    11
    //                ┌───┴───┐
    //              jake   pumpkin
    //
    // hash_00
    // = 01d2b46c3dd0180a5e8236137b4ada8ae6c9ca7c8799ecf7932d1320c9dfbf3b (same as v1)
    //
    // hash_01
    // = hash(01 | hash("donald") | hash("duck"))
    // = hash(01 | 4138cfbc5d36f31e8ae09ef4044bb88c0c9c6f289a6a1c27b335a99d1d8dc86f | 2d2370db2447ff8cf4f3accd68c85aa119a9c893effd200a9b69176e9fc5eb98)
    // = 44cb87f51dbe89d482329a5cc71fadf6758d3c3f7a46b8e03efbc9354e4b5be7
    //
    // hash_0
    // = hash(00 | 01d2b46c3dd0180a5e8236137b4ada8ae6c9ca7c8799ecf7932d1320c9dfbf3b | 44cb87f51dbe89d482329a5cc71fadf6758d3c3f7a46b8e03efbc9354e4b5be7)
    // = 7ce76869da6e1ff26f873924e6667e131761ef9075aebd6bba7c48663696f402
    //
    // hash_110
    // = 8358fe5d68c2d969c72b67ccffef68e2bf3b2edb200c0a7731e9bf131be11394 (same as in v1)
    //
    // hash_111
    // = hash(01 | hash("pumpkin") | hash("cat"))
    // = hash(01 | ff48e511e1638fc379cb75de1c28fe2016051b167f9aa8cac3dd86c6f4787539 | 77af778b51abd4a3c51c5ddd97204a9c3ae614ebccb75a606c3b6865aed6744e)
    // = a2cb2e0c6a5b3717d5355d1e8d046f305f7bd9730cf94434b51063209664f9c6
    //
    // hash_11
    // = hash(00 | 8358fe5d68c2d969c72b67ccffef68e2bf3b2edb200c0a7731e9bf131be11394 | a2cb2e0c6a5b3717d5355d1e8d046f305f7bd9730cf94434b51063209664f9c6)
    // = 1fd4c7d63c6349b827d1af289d9870f923d0be6ecbb6b91c2f42d81ac7b45a51
    //
    // hash_1
    // = hash(00 | 0000000000000000000000000000000000000000000000000000000000000000 | 1fd4c7d63c6349b827d1af289d9870f923d0be6ecbb6b91c2f42d81ac7b45a51)
    // = 9445f09716426120318220f103d9925c8a73155cf561ed4440b3d1fdc1f1153f
    //
    // root_hash
    // = hash(00 | 7ce76869da6e1ff26f873924e6667e131761ef9075aebd6bba7c48663696f402 | 9445f09716426120318220f103d9925c8a73155cf561ed4440b3d1fdc1f1153f)
    // = 05c5d1c5e433ed85c4b5c42d4da7adf6d204d3c1af37cac316f47b042c154eb4
    mod v1 {
        use super::*;

        pub const ROOT_HASH: Hash256 = Hash256::from_inner(hex!(
            "05c5d1c5e433ed85c4b5c42d4da7adf6d204d3c1af37cac316f47b042c154eb4"
        ));
        pub const HASH_0: Hash256 = Hash256::from_inner(hex!(
            "7ce76869da6e1ff26f873924e6667e131761ef9075aebd6bba7c48663696f402"
        ));
        pub const HASH_00: Hash256 = Hash256::from_inner(hex!(
            "01d2b46c3dd0180a5e8236137b4ada8ae6c9ca7c8799ecf7932d1320c9dfbf3b"
        ));
        pub const HASH_01: Hash256 = Hash256::from_inner(hex!(
            "44cb87f51dbe89d482329a5cc71fadf6758d3c3f7a46b8e03efbc9354e4b5be7"
        ));
        pub const HASH_1: Hash256 = Hash256::from_inner(hex!(
            "9445f09716426120318220f103d9925c8a73155cf561ed4440b3d1fdc1f1153f"
        ));
        pub const HASH_110: Hash256 = Hash256::from_inner(hex!(
            "8358fe5d68c2d969c72b67ccffef68e2bf3b2edb200c0a7731e9bf131be11394"
        ));
        pub const HASH_111: Hash256 = Hash256::from_inner(hex!(
            "a2cb2e0c6a5b3717d5355d1e8d046f305f7bd9730cf94434b51063209664f9c6"
        ));
    }

    #[test]
    fn disk_db_works() {
        let path = TempDataDir::new("_dango_disk_db_works");
        let db = DiskDb::<MerkleTree>::open(&path).unwrap();

        // Version 0
        {
            // Write a batch.
            let batch = Batch::from([
                (b"donald".to_vec(), Op::Insert(b"trump".to_vec())),
                (b"jake".to_vec(), Op::Insert(b"shepherd".to_vec())),
                (b"joe".to_vec(), Op::Insert(b"biden".to_vec())),
                (b"larry".to_vec(), Op::Insert(b"engineer".to_vec())),
            ]);
            let (version, root_hash) = db.flush_and_commit(batch).unwrap();
            assert_eq!(version, 0);
            assert_eq!(root_hash, Some(v0::ROOT_HASH));

            // Read single records.
            for (key, value) in [
                ("donald", Some("trump")),
                ("jake", Some("shepherd")),
                ("joe", Some("biden")),
                ("larry", Some("engineer")),
                ("pumpkin", None),
            ] {
                let found_value = db
                    .state_storage(Some(0))
                    .unwrap()
                    .read(key.as_bytes())
                    .map(|bz| String::from_utf8(bz).unwrap());
                assert_eq!(found_value.as_deref(), value);
            }

            // Iterator records.
            for ((found_key, found_value), (key, value)) in db
                .state_storage(Some(version))
                .unwrap()
                .scan(None, None, Order::Ascending)
                .zip([
                    ("donald", "trump"),
                    ("jake", "shepherd"),
                    ("joe", "biden"),
                    ("larry", "engineer"),
                ])
            {
                assert_eq!(found_key, key.as_bytes());
                assert_eq!(found_value, value.as_bytes());
            }
        }

        // Version 1
        {
            // Write a batch.
            let batch = Batch::from([
                (b"donald".to_vec(), Op::Insert(b"duck".to_vec())),
                (b"joe".to_vec(), Op::Delete),
                (b"pumpkin".to_vec(), Op::Insert(b"cat".to_vec())),
            ]);
            let (version, root_hash) = db.flush_and_commit(batch).unwrap();
            assert_eq!(version, 1);
            assert_eq!(root_hash, Some(v1::ROOT_HASH));

            // Read single records.
            for (key, value) in [
                ("donald", Some("duck")),
                ("jake", Some("shepherd")),
                ("joe", None),
                ("larry", Some("engineer")),
                ("pumpkin", Some("cat")),
            ] {
                let found_value = db
                    .state_storage(Some(1))
                    .unwrap()
                    .read(key.as_bytes())
                    .map(|bz| String::from_utf8(bz).unwrap());
                assert_eq!(found_value.as_deref(), value);
            }

            // Iterator records.
            for ((found_key, found_value), (key, value)) in db
                .state_storage(Some(version))
                .unwrap()
                .scan(None, None, Order::Ascending)
                .zip([
                    ("donald", "duck"),
                    ("jake", "shepherd"),
                    ("larry", "engineer"),
                    ("pumpkin", "cat"),
                ])
            {
                assert_eq!(found_key, key.as_bytes());
                assert_eq!(found_value, value.as_bytes());
            }
        }

        // Try generating merkle proofs at the two versions, respectively; also
        // verify the proofs.
        for (version, key, value, proof) in [
            (
                0,
                "donald",
                Some("trump"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v0::HASH_011), Some(v0::HASH_00), Some(v0::HASH_1)],
                }),
            ),
            (
                0,
                "jake",
                Some("shepherd"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v0::HASH_0)],
                }),
            ),
            (
                0,
                "joe",
                Some("biden"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v0::HASH_010), Some(v0::HASH_00), Some(v0::HASH_1)],
                }),
            ),
            (
                0,
                "larry",
                Some("engineer"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v0::HASH_01), Some(v0::HASH_1)],
                }),
            ),
            (
                0,
                "pumpkin",
                None,
                Proof::NonMembership(NonMembershipProof {
                    node: ProofNode::Leaf {
                        key_hash: "jake".sha2_256(),
                        value_hash: "shepherd".sha2_256(),
                    },
                    sibling_hashes: vec![Some(v0::HASH_0)],
                }),
            ),
            (
                1,
                "donald",
                Some("duck"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v1::HASH_00), Some(v1::HASH_1)],
                }),
            ),
            (
                1,
                "jake",
                Some("shepherd"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v1::HASH_111), None, Some(v1::HASH_0)],
                }),
            ),
            (
                1,
                "joe",
                None,
                Proof::NonMembership(NonMembershipProof {
                    node: ProofNode::Leaf {
                        key_hash: "donald".sha2_256(),
                        value_hash: "duck".sha2_256(),
                    },
                    sibling_hashes: vec![Some(v1::HASH_00), Some(v1::HASH_1)],
                }),
            ),
            (
                1,
                "larry",
                Some("engineer"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v1::HASH_01), Some(v1::HASH_1)],
                }),
            ),
            (
                1,
                "pumpkin",
                Some("cat"),
                Proof::Membership(MembershipProof {
                    sibling_hashes: vec![Some(v1::HASH_110), None, Some(v1::HASH_0)],
                }),
            ),
        ] {
            let found_proof = db.prove(key.as_bytes(), Some(version)).unwrap();
            assert_eq!(found_proof, proof);

            let root_hash = match version {
                0 => v0::ROOT_HASH,
                1 => v1::ROOT_HASH,
                _ => unreachable!(),
            };
            assert!(
                verify_proof(
                    root_hash,
                    key.as_bytes().sha2_256(),
                    value.map(|v| v.sha2_256()),
                    &found_proof,
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn disk_db_pruning_works() {
        let path = TempDataDir::new("_dango_disk_db_pruning_works");
        let db = DiskDb::<MerkleTree>::open(&path).unwrap();

        // Apply a few batches. Same test data as used in the JMT test.
        for batch in [
            // v0
            Batch::from([
                (b"r".to_vec(), Op::Insert(b"foo".to_vec())),
                (b"m".to_vec(), Op::Insert(b"bar".to_vec())),
                (b"L".to_vec(), Op::Insert(b"fuzz".to_vec())),
                (b"a".to_vec(), Op::Insert(b"buzz".to_vec())),
            ]),
            // v1
            Batch::from([(b"m".to_vec(), Op::Delete)]),
            // v2
            Batch::from([(b"r".to_vec(), Op::Delete)]),
            // v3
            Batch::from([(b"L".to_vec(), Op::Delete)]),
            // v4
            Batch::from([(b"a".to_vec(), Op::Delete)]),
        ] {
            db.flush_and_commit(batch).unwrap();
        }

        // Prune up to v3.
        // This deletes version 0-2. v3 is now the oldest available version.
        db.prune(3).unwrap();

        // Attempt access the state under versions 0..=2, should fail.
        for version in 0..=2 {
            // Request state storage. Should fail with `DbError::VersionTooOld`.
            assert!(db.state_storage(Some(version)).is_err_and(|err| {
                err.to_string()
                    .contains("doesn't equal the current version (4)")
            }));

            // Prove a key. Should fail when attempting to load the root node of
            // that version.
            assert!(db.prove(b"a", Some(version)).is_err_and(|err| {
                err.to_string()
                    .contains("data not found! type: dango_jmt::node::Node")
            }));
        }

        // State storage doesn't exist for version 3, because we only keep the
        // latest version, but proving at version 3 should work.
        // Proof doesn't work for version 4 though, because the tree is empty.
        // We can't proof anything if the tree is empty...
        {
            assert!(db.state_storage(Some(3)).is_err());
            assert!(db.prove(b"a", Some(3)).is_ok());
        }

        // Doing the same under version 5 (newer than the latest version) should fail.
        {
            assert!(db.state_storage(Some(5)).is_err_and(|err| {
                err.to_string()
                    .contains("doesn't equal the current version (4)")
            }));

            assert!(db.prove(b"a", Some(5)).is_err_and(|err| {
                err.to_string()
                    .contains("data not found! type: dango_jmt::node::Node")
            }));
        }
    }
}

// ----------------------- tests using simple commitment -----------------------

#[cfg(test)]
mod tests_simple {
    use {
        super::*,
        dango_app::{SimpleCommitment, StorageProvider},
        dango_primitives::{
            BorshSerExt, MockStorage, ResultExt, Shared, btree_map, btree_set, hash,
        },
        dango_storage::Map,
        dango_temp_rocksdb::TempDataDir,
    };

    // sha256(6 | donald | 1 | 5 | trump | 4 | jake | 1 | 8 | shepherd | 3 | joe | 1 | 5 | biden | 5 | larry | 1 | 8 | engineer)
    // = sha256(0006646f6e616c640100057472756d7000046a616b65010008736865706865726400036a6f65010005626964656e00056c61727279010008656e67696e656572)
    const V0_HASH: Hash256 =
        hash!("be33ce9316ee2af84f037db3a9d6d01bd2e61557ae7859d4d02138b08e6cc9f9");

    // sha256(6 | donald | 1 | 4 | duck | 3 | joe | 0 | 7 | pumpkin | 1 | 3 | cat)
    // = sha256(0006646f6e616c640100046475636b00036a6f6500000770756d706b696e010003636174)
    const V1_HASH: Hash256 =
        hash!("27fc5226bce75bd7750366ee3ddcf35f2d8daafb9f8e14f855f673e1e6fcb021");

    #[test]
    fn disk_db_lite_works() {
        let path = TempDataDir::new("_dango_disk_db_lite_works");
        let db = DiskDb::<SimpleCommitment>::open(&path).unwrap();

        // Write a 1st batch.
        {
            let batch = Batch::from([
                (b"donald".to_vec(), Op::Insert(b"trump".to_vec())),
                (b"jake".to_vec(), Op::Insert(b"shepherd".to_vec())),
                (b"joe".to_vec(), Op::Insert(b"biden".to_vec())),
                (b"larry".to_vec(), Op::Insert(b"engineer".to_vec())),
            ]);
            let (version, root_hash) = db.flush_and_commit(batch).unwrap();
            assert_eq!(version, 0);
            assert_eq!(root_hash, Some(V0_HASH));

            for (k, v) in [
                ("donald", Some("trump")),
                ("jake", Some("shepherd")),
                ("joe", Some("biden")),
                ("larry", Some("engineer")),
            ] {
                let found_value = db
                    .state_storage(Some(version))
                    .unwrap()
                    .read(k.as_bytes())
                    .map(|v| String::from_utf8(v).unwrap());
                assert_eq!(found_value.as_deref(), v);
            }
        }

        // Write a 2nd batch.
        {
            let batch = Batch::from([
                (b"donald".to_vec(), Op::Insert(b"duck".to_vec())),
                (b"joe".to_vec(), Op::Delete),
                (b"pumpkin".to_vec(), Op::Insert(b"cat".to_vec())),
            ]);
            let (version, root_hash) = db.flush_and_commit(batch).unwrap();
            assert_eq!(version, 1);
            assert_eq!(root_hash, Some(V1_HASH));

            for (k, v) in [
                ("donald", Some("duck")),
                ("jake", Some("shepherd")),
                ("joe", None),
                ("larry", Some("engineer")),
                ("pumpkin", Some("cat")),
            ] {
                let found_value = db
                    .state_storage(Some(version))
                    .unwrap()
                    .read(k.as_bytes())
                    .map(|v| String::from_utf8(v).unwrap());
                assert_eq!(found_value.as_deref(), v);
            }
        }
    }

    #[test]
    fn priority_data() {
        let path = TempDataDir::new("_dango_disk_db_priority_data");

        // First, open the DB _without_ priority data, and write some records.
        let db = DiskDb::<SimpleCommitment>::open(&path).unwrap();

        db.flush_and_commit(Batch::from([
            (b"000".to_vec(), Op::Insert(b"000".to_vec())),
            (b"001".to_vec(), Op::Insert(b"001".to_vec())),
            (b"002".to_vec(), Op::Insert(b"002".to_vec())),
            (b"003".to_vec(), Op::Insert(b"003".to_vec())),
            (b"004".to_vec(), Op::Insert(b"004".to_vec())),
        ]))
        .unwrap();

        // Enusre rockdb max is exclusive.
        {
            let data = db.data.read();

            assert_eq!(
                data.db
                    .iterator_cf_opt(
                        &cf_state_storage(&data.db),
                        new_read_options(Some(b"000"), Some(b"004")),
                        IteratorMode::Start,
                    )
                    .map(|res| {
                        let (k, v) = res.unwrap();
                        (k.to_vec(), v.to_vec())
                    })
                    .collect::<Vec<_>>(),
                [
                    (b"000".to_vec(), b"000".to_vec()),
                    (b"001".to_vec(), b"001".to_vec()),
                    (b"002".to_vec(), b"002".to_vec()),
                    (b"003".to_vec(), b"003".to_vec()),
                ]
            );

            assert_eq!(
                data.db
                    .iterator_cf_opt(
                        &cf_state_storage(&data.db),
                        new_read_options(Some(b"000"), Some(b"005")),
                        IteratorMode::Start,
                    )
                    .map(|res| {
                        let (k, v) = res.unwrap();
                        (k.to_vec(), v.to_vec())
                    })
                    .collect::<Vec<_>>(),
                [
                    (b"000".to_vec(), b"000".to_vec()),
                    (b"001".to_vec(), b"001".to_vec()),
                    (b"002".to_vec(), b"002".to_vec()),
                    (b"003".to_vec(), b"003".to_vec()),
                    (b"004".to_vec(), b"004".to_vec()),
                ]
            );
        }

        drop(db);

        // Open the DB again, _with_ priority data this time.
        let db =
            DiskDb::<SimpleCommitment>::open_with_priority(&path, Some((b"000", b"004"))).unwrap();

        // In order to test the priorty data, we manually delete a key from the db.
        // This is not something that should happen in a normal use case but allows
        // us to test the priority data.
        // Remove the `002` key from the db.
        {
            let data = db.data.read();

            assert_eq!(
                data.db
                    .get_cf_opt(
                        &cf_state_storage(&data.db),
                        b"002",
                        &new_read_options(None, None)
                    )
                    .unwrap()
                    .unwrap(),
                b"002"
            );

            data.db
                .delete_cf(&cf_state_storage(&data.db), b"002")
                .unwrap();

            assert!(
                data.db
                    .get_cf_opt(
                        &cf_state_storage(&data.db),
                        b"002",
                        &new_read_options(None, None)
                    )
                    .unwrap()
                    .is_none()
            );
        }

        let storage = db.state_storage(None).unwrap();

        // We should be able to read `002` from the priority data.
        // This proves we're accessing it from priority data (as expected), not
        // from the underlying rocksdb.
        assert_eq!(storage.read(b"002").unwrap(), b"002");

        // Iterate over the a range included in the priority data
        assert_eq!(
            storage
                .scan_values(Some(b"001"), Some(b"003"), Order::Ascending)
                .collect::<Vec<_>>(),
            [b"001", b"002"]
        );

        // Iterate over the exact range of the priority data
        assert_eq!(
            storage
                .scan_values(Some(b"000"), Some(b"004"), Order::Ascending)
                .collect::<Vec<_>>(),
            [b"000", b"001", b"002", b"003"]
        );

        // Iterate over a range that exceeds the priority data.
        // Data are loaded from disk, key `002` should not be found.
        assert_eq!(
            storage
                .scan_values(Some(b"000"), Some(b"005"), Order::Ascending)
                .collect::<Vec<_>>(),
            [b"000", b"001", b"003", b"004"]
        );
    }

    #[test]
    fn priority_data_new_db() {
        let path = TempDataDir::new("_dango_disk_db_priority_data_new_db");

        // Open a brand new DB with priority data. Should succeed.
        let _db =
            DiskDb::<SimpleCommitment>::open_with_priority(&path, Some((b"000", b"004"))).unwrap();
    }

    #[test]
    fn ensure_wasm_cf_prefix() {
        const MAP: Map<&str, u64> = Map::new("map");

        let buffer = Shared::new(Buffer::new_unnamed(MockStorage::new(), None));

        let mut provider = StorageProvider::new(
            Box::new(buffer.clone()),
            &[CONTRACT_NAMESPACE, &Addr::mock(0)],
        );

        MAP.save(&mut provider, "foo", &1).unwrap();

        drop(provider);

        let (_, b) = buffer.disassemble().disassemble();

        let mut k = Vec::with_capacity(4 + 20 + 2 + 3 + 3);
        k.extend_from_slice(b"wasm");
        k.extend_from_slice(&Addr::mock(0));
        k.extend_from_slice(&("map".len() as u16).to_be_bytes());
        k.extend_from_slice(b"map");
        k.extend_from_slice(b"foo");

        assert_eq!(
            b,
            btree_map! { k => Op::Insert(1_u64.to_borsh_vec().unwrap()) }
        );
    }

    #[test]
    fn merge_iterators() {
        // Ascending
        let l = vec![1, 2, 4];
        let r = vec![3, 5, 6];

        assert_eq!(
            l.into_iter()
                .merge_by(r.into_iter(), |a, b| a < b)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6]
        );

        // Descending
        let l = vec![4, 2, 1];
        let r = vec![6, 5, 3];

        assert_eq!(
            l.into_iter()
                .merge_by(r.into_iter(), |a, b| a > b)
                .collect::<Vec<_>>(),
            [6, 5, 4, 3, 2, 1]
        );
    }

    /// The logic of the DB assumes records in the wasm column family have a 24-byte
    /// prefix (= 4 bytes of "wasm" + 20 bytes of contract address).
    /// We need to make sure this is true (in case a change in `StorageProvider`
    /// changes how storage is laid out).
    #[test]
    fn storage_provider_prefix_length_is_correct() {
        assert_eq!(
            StorageProvider::new(
                Box::new(MockStorage::new()),
                &[CONTRACT_NAMESPACE, &Addr::mock(0),]
            )
            .namespace()
            .len(),
            WASM_PREFIX_LEN,
            "storage provider prefix length is not the expected value"
        );
    }

    /// Should throw error if the batch contains a key that starts with `b"wasm"`
    /// but doesn't have the expected format of wasm keys.
    #[test]
    fn rejecting_non_wasm_key() {
        // Prepare a key that is not a wasm key. The contract address should be
        // 20 bytes, but here we only have 10 bytes.
        let not_wasm = concat(&[CONTRACT_NAMESPACE, &[0; 10], b"foo"]);
        assert!(!is_wasm_key(&not_wasm));

        let path = TempDataDir::new("_dango_disk_db_lite_iterator_works");
        let db = DiskDb::<SimpleCommitment>::open(&path).unwrap();

        db.flush_but_not_commit(btree_map! { not_wasm.clone() => Op::Delete })
            .should_fail_with_error(DbError::not_wasm_key(not_wasm));
    }

    #[test]
    fn iterator_works() {
        // -------------------------- prepare the DB ---------------------------

        let wasm_1 = concat(&[CONTRACT_NAMESPACE, &[1; 20], b"foo"]);
        assert!(is_wasm_key(&wasm_1));

        let wasm_3 = concat(&[CONTRACT_NAMESPACE, &[3; 20], b"foo"]);
        assert!(is_wasm_key(&wasm_3));

        let keys = btree_set! {
            b"foo".to_vec(),    // A record smaller than "wasm".
            wasm_1,             // A record in wasm contract 0x11..11.
            wasm_3,             // A record in wasm contract 0x33..33.
            b"xxxfoo".to_vec(), // A record bigger than "wasn".
        };

        let path = TempDataDir::new("_dango_disk_db_lite_iterator_works");
        let db = DiskDb::<SimpleCommitment>::open(&path).unwrap();

        let batch = keys
            .iter()
            .map(|k| (k.clone(), Op::Insert(b"bar".to_vec())))
            .collect();
        let (version, _root_hash) = db.flush_and_commit(batch).unwrap();

        let storage = db.state_storage(Some(version)).unwrap();

        // -------------------- iterate all possible cases ---------------------

        let min_not_wasm = concat(&[CONTRACT_NAMESPACE, &[1; 10]]);
        assert!(!is_wasm_key(&min_not_wasm));

        let min_wasm = concat(&[CONTRACT_NAMESPACE, &[1; 20]]);
        assert!(is_wasm_key(&min_wasm));

        let max_not_wasm = concat(&[CONTRACT_NAMESPACE, &[2; 10]]);
        assert!(!is_wasm_key(&max_not_wasm));

        let max_wasm = concat(&[CONTRACT_NAMESPACE, &[2; 20]]);
        assert!(is_wasm_key(&max_wasm));

        for (min, max) in [
            // max < wasm
            (None, Some(b"fuzz".as_slice())),
            // min < wasm < max (NOT a wasm key)
            (None, Some(max_not_wasm.as_slice())),
            // min < wasm < max (a wasm key)
            (None, Some(max_wasm.as_slice())),
            // min < wasm < wasn < max
            (None, None),
            // wasm < min (NOT a wasm key) < max (NOT a wasm key) < wasn
            (Some(min_not_wasm.as_slice()), Some(max_not_wasm.as_slice())),
            // wasm < min (NOT a wasm key) < max (a wasm key) < wasn
            (Some(min_not_wasm.as_slice()), Some(max_wasm.as_slice())),
            // wasm < min (a wasm key) < max (NOT a wasm key) < wasn
            (Some(min_wasm.as_slice()), Some(max_not_wasm.as_slice())),
            // wasm < min (a wasm key) < max (a wasm key) < wasn
            (Some(min_wasm.as_slice()), Some(max_wasm.as_slice())),
            // wasm < min (NOT a wasm key) < wasn < max
            (Some(min_not_wasm.as_slice()), None),
            // wasm < min (a wasm key) < wasn < max
            (Some(min_wasm.as_slice()), None),
            // wasn < min
            (Some(b"wb".as_slice()), None),
        ] {
            let lower = match min {
                Some(s) => Bound::Included(s),
                None => Bound::Unbounded,
            };

            let upper = match max {
                Some(s) => Bound::Excluded(s),
                None => Bound::Unbounded,
            };

            let expect = keys
                .range::<[u8], _>((lower, upper))
                .cloned()
                .collect::<Vec<_>>();

            let actual = storage
                .scan_keys(min, max, Order::Ascending)
                .collect::<Vec<_>>();

            assert_eq!(
                actual, expect,
                "actual and expect do not match! min = {min:?}, max = {max:?}"
            );
        }
    }

    fn concat(slices: &[&[u8]]) -> Vec<u8> {
        let len = slices.iter().map(|slice| slice.len()).sum();
        let mut vec = Vec::with_capacity(len);
        for slice in slices {
            vec.extend_from_slice(slice);
        }
        vec
    }
}

// ------------------------------- deadlock test -------------------------------

/// Reproduction of a deadlock issue we discovered in testnet-3.
///
/// Assume thread 1 represents the HTTPD server, thread 2 represents the ABCI
/// server:
///
/// - Thread 1 serves a query that involves iterateing over an `IndexedMap`.
///   The iterator acquires a read-lock to the `priority_data` map, and holds
///   onto it until the iterator itself is dropped.
/// - Thread 2 receives a `Commit` request from CometBFT. It attempts to acquire
///   a write-lock of `priority_data`.
/// - The iterator in thread 1 advances. Note that in `IndexedMap`, iteration
///   involves first reading an index key from the index map (this uses the
///   iterator's own read-lock), and then read from the primary map. To read
///   from the primary map, the iterator attempts to acquire another read-lock.
///   However, since thread 2 is already waiting for a write lock, according to
///   the fairness rule, thread 1 has to wait after thread 2. Hence, deadlock.
#[cfg(test)]
mod test_deadlock {
    use {
        super::*,
        dango_app::SimpleCommitment,
        dango_storage::{Index, IndexList, IndexedMap, MultiIndex},
        dango_temp_rocksdb::TempDataDir,
        std::time::Duration,
    };

    const PERSONS: IndexedMap<String, String, PersonIndexes> = IndexedMap::new(
        "person",
        PersonIndexes {
            race: MultiIndex::new(|_name, race| race.clone(), "person", "person__race"),
        },
    );

    struct PersonIndexes<'a> {
        pub race: MultiIndex<'a, String, String, String>,
    }

    impl<'a> IndexList<String, String> for PersonIndexes<'a> {
        fn get_indexes(&self) -> Box<dyn Iterator<Item = &'_ dyn Index<String, String>> + '_> {
            let v: [&dyn Index<String, String>; 1] = [&self.race];
            Box::new(v.into_iter())
        }
    }

    #[test]
    fn deadlock_problem() {
        with_timeout(run_deadlock_test, Duration::from_secs(5));
    }

    /// Ensure the execution of a function doesn't take longer than the given timeout.
    fn with_timeout<F>(f: F, timeout: Duration)
    where
        F: FnOnce() + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            f();
            tx.send(()).unwrap();
        });

        rx.recv_timeout(timeout).unwrap();
    }

    fn run_deadlock_test() {
        let path = TempDataDir::new("_dango_disk_db_deadlock_problem");
        let db = DiskDb::<SimpleCommitment>::open_with_priority(
            &path,
            Some(([0], [255])), // Simply use 0..255 as the priority range, so ALL data in the state storage is loaded into priority data.
        )
        .unwrap();

        // Batch 1: write some data into the index map.
        {
            let storage = db.state_storage(None).unwrap();
            let mut buffer = Buffer::new_unnamed(storage, None);

            for (name, race) in [
                ("Ulfric Stormcloak", "Nord"),
                ("General Tullius", "Imperial"),
                ("Kodlak Whitemane", "Nord"),
                ("Savos Aren", "Dark Elf"),
                ("Mercer Frey", "Breton"),
                ("Astrid", "Nord"),
            ] {
                PERSONS
                    .save(&mut buffer, name.to_string(), &race.to_string())
                    .unwrap();
            }

            let (_, batch) = buffer.disassemble();
            db.flush_and_commit(batch).unwrap();
        }

        // Spawn two threads:
        // - a "read thread" that mimics the httpd server;
        // - a "write thread" that mimics the ABCI server.
        std::thread::scope(|s| {
            // Create a thread to iterate through the map.
            let read_thread = s.spawn(|| {
                let storage = db.state_storage(None).unwrap();
                PERSONS
                    .idx
                    .race
                    .prefix("Nord".to_string())
                    .range(&storage, None, None, Order::Ascending) // Deadlock only happens with `range` and `values`, not with `keys`.
                    .map(|res| {
                        // Do the iteration slowly.
                        std::thread::sleep(Duration::from_secs(1));

                        let (name, _race) = res.unwrap();
                        name
                    })
                    .collect::<Vec<_>>()
            });

            // Create another thread to write a new batch.
            let write_thread = s.spawn(|| {
                let storage = db.state_storage(None).unwrap();
                let mut buffer = Buffer::new_unnamed(storage, None);

                for (name, race) in [
                    ("Aela the Huntress", "Nord"),
                    ("Urag gro-Shub", "Orc"),
                    ("Brynjolf", "Nord"),
                    ("Farengar Secret-Fire", "Nord"),
                    ("Balimund", "Nord"),
                    ("Delphine", "Breton"),
                ] {
                    PERSONS
                        .save(&mut buffer, name.to_string(), &race.to_string())
                        .unwrap();
                }

                let (_, batch) = buffer.disassemble();
                db.flush_but_not_commit(batch).unwrap();

                // Wait a second, so that we commit when the read thread is half way
                // through the iteration.
                std::thread::sleep(Duration::from_secs(1));

                db.commit().unwrap();
            });

            let names = read_thread.join().unwrap();
            write_thread.join().unwrap();

            // Names should only include those in batch 1, without those in batch 2.
            assert_eq!(names, ["Astrid", "Kodlak Whitemane", "Ulfric Stormcloak"]);
        });

        // Read again. The records in batch 2 should now be included.
        {
            let storage = db.state_storage(None).unwrap();
            let names = PERSONS
                .idx
                .race
                .prefix("Nord".to_string())
                .range(&storage, None, None, Order::Ascending)
                .map(|res| {
                    let (name, _race) = res.unwrap();
                    name
                })
                .collect::<Vec<_>>();

            assert_eq!(
                names,
                [
                    "Aela the Huntress",
                    "Astrid",
                    "Balimund",
                    "Brynjolf",
                    "Farengar Secret-Fire",
                    "Kodlak Whitemane",
                    "Ulfric Stormcloak",
                ]
            );
        }
    }
}
