use {
    crate::{Borsh, Codec, Index, Key, Map},
    grug_types::{StdError, StdResult, Storage},
    std::ops::Deref,
};

/// An indexer that ensures that indexes are unique, meaning no two records in
/// the primary map may have the same index.
///
/// Internally, a `UniqueIndex` is a wrapper around a `Map` that maps index keys
/// to values. Essentially, when a key-value pair is stored in the `Map`, the
/// value is stored twice:
///
/// - In the primary map: (pk_namespace, pk) => value
/// - in the index map: (idx_namespace, ik) => value
pub struct UniqueIndexMap<'a, IK, T, C: Codec<T> = Borsh> {
    /// A function that takes a piece of data, and return the index key it
    /// should be indexed at.
    indexer: fn(&T) -> IK,
    /// Data indexed by the index key.
    idx_map: Map<'a, IK, T, C>,
}

impl<'a, IK, T, C> UniqueIndexMap<'a, IK, T, C>
where
    C: Codec<T>,
{
    /// Note: The developer must make sure that `idx_namespace` is not the same
    /// as the primary map namespace.
    pub const fn new(indexer: fn(&T) -> IK, idx_namespace: &'static str) -> Self {
        UniqueIndexMap {
            indexer,
            idx_map: Map::new(idx_namespace),
        }
    }
}

// Since the `UniqueIndex` is essentially a wrapper of a `Map` (`self.idx_map`),
// we let it dereference to the inner map. This way, users are able to directly
// call methods on the inner map, such as `range`, `prefix`, etc.
impl<'a, IK, T, C> Deref for UniqueIndexMap<'a, IK, T, C>
where
    C: Codec<T>,
{
    type Target = Map<'a, IK, T, C>;

    fn deref(&self) -> &Self::Target {
        &self.idx_map
    }
}

impl<'a, PK, IK, T, C> Index<PK, T> for UniqueIndexMap<'a, IK, T, C>
where
    IK: Key + Clone,
    C: Codec<T>,
    T: Clone,
{
    fn save(&self, storage: &mut dyn Storage, _pk: PK, data: &T) -> StdResult<()> {
        let idx = (self.indexer)(data);

        // Ensure that indexes are unique.
        if self.idx_map.has(storage, idx.clone()) {
            // TODO: create a `StdError::DuplicateData` for this?
            return Err(StdError::generic_err("Violates unique constraint on index"));
        }

        self.idx_map.save(storage, idx, data)
    }

    fn remove(&self, storage: &mut dyn Storage, _pk: PK, old_data: &T) {
        let idx = (self.indexer)(old_data);
        self.idx_map.remove(storage, idx);
    }
}
