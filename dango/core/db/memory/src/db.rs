use {
    crate::{DbError, DbResult},
    dango_app::{Commitment, Db, SimpleCommitment},
    dango_primitives::{
        Batch, Buffer, Hash256, HashExt, MockStorage, Op, Order, Record, Shared, Storage,
    },
    ouroboros::self_referencing,
    parking_lot::RwLockReadGuard,
    std::{collections::BTreeMap, marker::PhantomData},
};
#[cfg(feature = "snapshot")]
use {
    borsh::{BorshDeserialize, BorshSerialize},
    dango_primitives::{BorshDeExt, BorshSerExt},
    std::{fs, path::Path},
};

#[cfg_attr(feature = "snapshot", derive(BorshSerialize, BorshDeserialize))]
struct ChangeSet {
    version: u64,
    state_commitment: Batch,
    state_storage: Batch,
}

#[cfg_attr(feature = "snapshot", derive(BorshSerialize, BorshDeserialize))]
struct MemDbInner {
    /// Version of the DB. Initilialized to `None` when the DB instance is
    /// created. Set of 0 the first time a batch of data is committed, and
    /// incremented by 1 each time afterwards.
    latest_version: Option<u64>,
    /// The root hash of the last committed batch.
    last_root_hash: Option<Hash256>,
    /// A key-value store backing the Merkle tree.
    state_commitment: MockStorage,
    /// A key-value storage: key => value.
    /// Note that unlike `DiskDb`, we don't store historical states.
    state_storage: MockStorage,
    /// Uncommitted changes
    changeset: Option<ChangeSet>,
}

pub struct MemDb<T = SimpleCommitment> {
    inner: Shared<MemDbInner>,
    _commitment: PhantomData<T>,
}

impl<T> MemDb<T> {
    pub fn new() -> Self {
        Self {
            inner: Shared::new(MemDbInner {
                latest_version: None,
                last_root_hash: None,
                state_commitment: BTreeMap::new(),
                state_storage: BTreeMap::new(),
                changeset: None,
            }),
            _commitment: PhantomData,
        }
    }

    /// Get direct immutable access to the state storage.
    pub fn with_state_storage<C, R>(&self, callback: C) -> R
    where
        C: FnOnce(&dyn Storage) -> R,
    {
        self.inner
            .write_with(|inner| callback(&inner.state_storage))
    }

    /// Get direct mutable access to the state storage. Only intended for testing
    /// and debugging purposes.
    pub fn with_state_storage_mut<C, R>(&self, callback: C) -> R
    where
        C: FnOnce(&mut dyn Storage) -> R,
    {
        self.inner
            .write_with(|mut inner| callback(&mut inner.state_storage))
    }
}

#[cfg(feature = "snapshot")]
impl<T> MemDb<T> {
    /// Dump the database to a file.
    pub fn dump<P>(&self, path: P) -> anyhow::Result<()>
    where
        P: AsRef<Path>,
    {
        let bytes = self.inner.read_with(|inner| inner.to_borsh_vec())?;

        Ok(fs::write(path, bytes)?)
    }

    /// Recover the database from a file.
    pub fn recover<P>(path: P) -> anyhow::Result<Self>
    where
        P: AsRef<Path>,
    {
        let bytes = fs::read(path)?;
        let inner = bytes.deserialize_borsh()?;

        Ok(Self {
            inner: Shared::new(inner),
            _commitment: PhantomData,
        })
    }
}

impl<T> Default for MemDb<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for MemDb<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _commitment: PhantomData,
        }
    }
}

impl<T> Db for MemDb<T>
where
    T: Commitment,
{
    type Error = DbError;
    type Proof = T::Proof;
    type StateCommitment = StateCommitment;
    type StateStorage = StateStorage;

    fn state_commitment(&self) -> Self::StateCommitment {
        StateCommitment {
            inner: self.inner.clone(),
        }
    }

    fn state_storage_with_comment(
        &self,
        version: Option<u64>,
        _comment: &'static str,
    ) -> DbResult<StateStorage> {
        if let Some(requested) = version {
            let db_version = self.latest_version().unwrap_or(0);
            if requested != db_version {
                return Err(DbError::incorrect_version(db_version, requested));
            }
        }

        Ok(StateStorage {
            inner: self.inner.clone(),
        })
    }

    fn latest_version(&self) -> Option<u64> {
        self.inner.read_with(|inner| inner.latest_version)
    }

    fn oldest_version(&self) -> Option<u64> {
        // We only store the latest version, so the latest version is also the
        // latest version.
        self.latest_version()
    }

    fn root_hash(&self, version: Option<u64>) -> DbResult<Option<Hash256>> {
        self.inner.read_with(|inner| {
            if inner.latest_version != version {
                return Ok(None);
            }

            Ok(inner.last_root_hash)
        })
    }

    fn prove(&self, key: &[u8], version: Option<u64>) -> DbResult<Self::Proof> {
        let version = version.unwrap_or_else(|| self.latest_version().unwrap_or(0));
        Ok(T::prove(&self.state_commitment(), key.sha2_256(), version)?)
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
    fn flush_but_not_commit(&self, batch: Batch) -> DbResult<(u64, Option<Hash256>)> {
        let (version, root_hash, state_commitment) = self.inner.read_with(|inner| {
            if inner.changeset.is_some() {
                return Err(DbError::change_set_already_set());
            }

            let (old_version, new_version) =
                inner.latest_version.map(|v| (v, v + 1)).unwrap_or((0, 0));

            let mut buffer = Buffer::new(
                self.state_commitment(),
                None,
                "mem_db/state_commitment/flush_but_not_commit",
            );

            let root_hash = T::apply(&mut buffer, old_version, new_version, &batch)?;
            let (_, changeset) = buffer.disassemble();

            Ok((new_version, root_hash, changeset))
        })?;

        self.inner.write_with(|mut inner| {
            inner.changeset = Some(ChangeSet {
                version,
                state_commitment,
                state_storage: batch,
            });
        });

        Ok((version, root_hash))
    }

    fn commit(&self) -> DbResult<u64> {
        self.inner.write_with(|mut inner| {
            let changeset = inner
                .changeset
                .take()
                .ok_or(DbError::change_set_not_set())?;

            // Update the version
            inner.latest_version = Some(changeset.version);

            // Write changes to state commitment
            for (key, op) in changeset.state_commitment {
                if let Op::Insert(value) = op {
                    inner.state_commitment.insert(key, value);
                } else {
                    inner.state_commitment.remove(&key);
                }
            }

            // Write changes to state storage
            for (key, op) in changeset.state_storage {
                if let Op::Insert(value) = op {
                    inner.state_storage.insert(key, value);
                } else {
                    inner.state_storage.remove(&key);
                }
            }

            Ok(changeset.version)
        })
    }

    fn prune(&self, up_to_version: u64) -> Result<(), Self::Error> {
        // State storage only contains the latest height, so no need to prune.
        // Only prune the state commitment.
        Ok(T::prune(&mut self.state_commitment(), up_to_version)?)
    }
}

// ----------------------------- state commitment ------------------------------

#[derive(Clone)]
pub struct StateCommitment {
    inner: Shared<MemDbInner>,
}

impl Storage for StateCommitment {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.inner
            .read_with(|inner| inner.state_commitment.get(key).cloned())
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
}

// ------------------------------- state storage -------------------------------

#[derive(Clone)]
pub struct StateStorage {
    inner: Shared<MemDbInner>,
}

impl Storage for StateStorage {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.inner
            .read_with(|inner| inner.state_storage.get(key).cloned())
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        Box::new(SharedIter::new(self.inner.read_access(), |inner| {
            inner.state_storage.scan(min, max, order)
        }))
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        Box::new(SharedIter::new(self.inner.read_access(), |inner| {
            inner.state_storage.scan_keys(min, max, order)
        }))
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        Box::new(SharedIter::new(self.inner.read_access(), |inner| {
            inner.state_storage.scan_values(min, max, order)
        }))
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

// -------------------------------- shared iter --------------------------------

// Note: we can't use the `SharedIter` defined in dango-primitives, because the `SharedIter::new`
// method generated by the `#[self_referencing]` macro isn't public.
#[self_referencing]
pub struct SharedIter<'a, S, T> {
    guard: RwLockReadGuard<'a, S>,
    #[borrows(guard)]
    #[covariant]
    inner: Box<dyn Iterator<Item = T> + 'this>,
}

impl<'a, S, T> Iterator for SharedIter<'a, S, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.with_inner_mut(|iter| iter.next())
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod test {
    // TODO
}
