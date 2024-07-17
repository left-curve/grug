use {
    crate::{DbError, DbResult, VersionedMap},
    grug_app::{Buffer, Db},
    grug_jmt::{MerkleTree, Proof},
    grug_types::{hash, Batch, Hash, Op, Order, Record, Storage},
    std::{
        collections::HashMap,
        ops::Bound,
        sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    },
};

const MERKLE_TREE: MerkleTree = MerkleTree::new_default();

struct ChangeSet {
    version: u64,
    state_commitment: Batch,
    state_storage: Batch,
}

struct MemDbInner {
    /// Version of the DB. Initilialized to `None` when the DB instance is
    /// created. Set of 0 the first time a batch of data is committed, and
    /// incremented by 1 each time afterwards.
    latest_version: Option<u64>,
    /// A key-value store backing the Merkle tree.
    ///
    /// A HashMap is chosen over BTreeMap because our Merkle tree implementation
    /// does not need to iterate raw keys in this store.
    state_commitment: HashMap<Vec<u8>, Vec<u8>>,
    /// A versioned key-value storage: key => (version => value)
    state_storage: VersionedMap<Vec<u8>, Vec<u8>>,
    /// Uncommitted changes
    changeset: Option<ChangeSet>,
}

pub struct MemDb {
    inner: Arc<RwLock<MemDbInner>>,
}

impl MemDb {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(MemDbInner {
                latest_version: None,
                state_commitment: HashMap::new(),
                state_storage: VersionedMap::new(),
                changeset: None,
            })),
        }
    }

    fn with_read<C, T>(&self, callback: C) -> T
    where
        C: FnOnce(RwLockReadGuard<MemDbInner>) -> T,
    {
        let lock = self.inner.read().unwrap_or_else(|err| {
            panic!("MemDb is poisoned: {err:?}");
        });
        callback(lock)
    }

    fn with_write<C, T>(&self, callback: C) -> T
    where
        C: FnOnce(RwLockWriteGuard<MemDbInner>) -> T,
    {
        let lock = self.inner.write().unwrap_or_else(|err| {
            panic!("MemDb is poisoned: {err:?}");
        });
        callback(lock)
    }
}

impl Default for MemDb {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for MemDb {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Db for MemDb {
    type Error = DbError;
    type Proof = Proof;

    fn state_commitment(&self) -> impl Storage + Clone + 'static {
        StateCommitment { db: self.clone() }
    }

    fn state_storage(&self, version: Option<u64>) -> impl Storage + Clone + 'static {
        StateStorage {
            db: self.clone(),
            version: version.unwrap_or_else(|| self.latest_version().unwrap_or(0)),
        }
    }

    fn latest_version(&self) -> Option<u64> {
        self.with_read(|inner| inner.latest_version)
    }

    fn root_hash(&self, version: Option<u64>) -> DbResult<Option<Hash>> {
        let version = version.unwrap_or_else(|| self.latest_version().unwrap_or(0));
        Ok(MERKLE_TREE.root_hash(&self.state_commitment(), version)?)
    }

    fn prove(&self, key: &[u8], version: Option<u64>) -> DbResult<Proof> {
        let version = version.unwrap_or_else(|| self.latest_version().unwrap_or(0));
        Ok(MERKLE_TREE.prove(&self.state_commitment(), &hash(key), version)?)
    }

    // Note on implementing this function: We must make sure that we don't
    // attempt to lock the DB (either read or write) inside the `with_write`
    // callback. Doing so will result in error:
    //
    // > rwlock read lock would result in deadlock
    //
    // The best way to avoid this is to do everything that requires a read lock
    // first (using a `with_read` callback) and do everything that requires a
    // write lock in the end (using a `with_write` callback).
    fn flush_but_not_commit(&self, batch: Batch) -> DbResult<(u64, Option<Hash>)> {
        let (new_version, root_hash, changeset) = self.with_read(|inner| {
            if inner.changeset.is_some() {
                return Err(DbError::ChangeSetAlreadySet);
            }

            let (old_version, new_version) = match self.latest_version() {
                Some(v) => (v, v + 1),
                None => (0, 0),
            };

            let mut cache = Buffer::new(self.state_commitment(), None);
            let root_hash = MERKLE_TREE.apply_raw(&mut cache, old_version, new_version, &batch)?;
            let (_, changeset) = cache.disassemble();

            Ok((new_version, root_hash, changeset))
        })?;

        self.with_write(|mut inner| {
            inner.changeset = Some(ChangeSet {
                version: new_version,
                state_commitment: changeset,
                state_storage: batch,
            });
        });

        Ok((new_version, root_hash))
    }

    fn commit(&self) -> DbResult<()> {
        self.with_write(|mut inner| {
            let changeset = inner.changeset.take().ok_or(DbError::ChangeSetNotSet)?;

            // update the version
            inner.latest_version = Some(changeset.version);

            // write changes to state commitment
            for (key, op) in changeset.state_commitment {
                if let Op::Insert(value) = op {
                    inner.state_commitment.insert(key, value);
                } else {
                    inner.state_commitment.remove(&key);
                }
            }

            // write changes to state storage
            inner.state_storage.write_batch(changeset.state_storage);

            Ok(())
        })
    }

    // TODO: add a more performant implementation of `flush_and_commit`
}

// ----------------------------- state commitment ------------------------------

#[derive(Clone)]
pub struct StateCommitment {
    db: MemDb,
}

impl Storage for StateCommitment {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.db
            .with_read(|inner| inner.state_commitment.get(key).cloned())
    }

    fn scan<'a>(
        &'a self,
        _min: Option<&[u8]>,
        _max: Option<&[u8]>,
        _order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        unimplemented!("this isn't used by the Merkle tree");
    }

    fn scan_keys<'a>(
        &'a self,
        _min: Option<&[u8]>,
        _max: Option<&[u8]>,
        _order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        unimplemented!("this isn't used by the Merkle tree");
    }

    fn scan_values<'a>(
        &'a self,
        _min: Option<&[u8]>,
        _max: Option<&[u8]>,
        _order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        unimplemented!("this isn't used by the Merkle tree");
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

    fn scan_sized<'a>(
        &'a self,
        _min: Option<&[u8]>,
        _max: Option<&[u8]>,
        _order: Order,
        _size: u32,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        unimplemented!("this isn't used by the Merkle tree");
    }
}

// ------------------------------- state storage -------------------------------

#[derive(Clone)]
pub struct StateStorage {
    db: MemDb,
    version: u64,
}

impl Storage for StateStorage {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.db
            .with_read(|inner| inner.state_storage.get(key, self.version).cloned())
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let min = min.map_or(Bound::Unbounded, Bound::Included);
        let max = max.map_or(Bound::Unbounded, Bound::Excluded);
        let vec = self.db.with_read(|inner| {
            // Here we must collect the iterator into a `Vec`, because the
            // iterator only lives as longa as the read lock, which goes out of
            // scope at the end of the function.
            inner
                .state_storage
                .range::<_, [u8]>((min, max), self.version)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        });
        match order {
            Order::Ascending => Box::new(vec.into_iter()),
            Order::Descending => Box::new(vec.into_iter().rev()),
        }
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        // Here we take the approach of iterating both the keys and the values,
        // and simply discard the values. Apparently this isn't efficient since
        // in `scan` we clone the values for no purpose. This said, db/memory is
        // for running tests only, so this is ok.
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

    fn scan_sized<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
        size: u32,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        // TODO: optimize this??
        Box::new(self.scan(min, max, order).take(size as usize))
    }
}
