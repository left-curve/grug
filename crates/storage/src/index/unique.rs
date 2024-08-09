use {
    crate::{Borsh, Codec, Index, Key, Map},
    borsh::{BorshDeserialize, BorshSerialize},
    grug_types::{StdError, StdResult, Storage},
    std::{fmt::Debug, marker::PhantomData, ops::Deref},
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
pub struct UniqueIndex<'a, PK, IK, T, C = Borsh>
where
    C: Codec<T> + Codec<UniqueValue<PK, T>>,
{
    /// A function that takes a piece of data, and return the index key it
    /// should be indexed at.
    indexer: fn(&T) -> IK,
    /// Data indexed by the index key.
    idx_map: Map<'a, IK, UniqueValue<PK, T>, C>,
}

impl<'a, PK, IK, T, C> UniqueIndex<'a, PK, IK, T, C>
where
    C: Codec<T> + Codec<UniqueValue<PK, T>>,
{
    /// Note: The developer must make sure that `idx_namespace` is not the same
    /// as the primary map namespace.
    pub const fn new(indexer: fn(&T) -> IK, idx_namespace: &'static str) -> Self {
        UniqueIndex {
            indexer,
            idx_map: Map::new(idx_namespace),
        }
    }
}

// Since the `UniqueIndex` is essentially a wrapper of a `Map` (`self.idx_map`),
// we let it dereference to the inner map. This way, users are able to directly
// call methods on the inner map, such as `range`, `prefix`, etc.
impl<'a, PK, IK, T, C> Deref for UniqueIndex<'a, PK, IK, T, C>
where
    C: Codec<T> + Codec<UniqueValue<PK, T>>,
{
    type Target = Map<'a, IK, UniqueValue<PK, T>, C>;

    fn deref(&self) -> &Self::Target {
        &self.idx_map
    }
}

impl<'a, PK, IK, T, C> Index<PK, T> for UniqueIndex<'a, PK, IK, T, C>
where
    IK: Key + Clone,
    PK: Key,
    C: Codec<T> + Codec<UniqueValue<PK, T>>,
    T: Clone,
{
    fn save(&self, storage: &mut dyn Storage, pk: PK, data: &T) -> StdResult<()> {
        let idx = (self.indexer)(data);

        // Ensure that indexes are unique.
        if self.idx_map.has(storage, idx.clone()) {
            // TODO: create a `StdError::DuplicateData` for this?
            return Err(StdError::generic_err("Violates unique constraint on index"));
        }

        let value = UniqueValue::new(pk, data.clone());

        self.idx_map.save(storage, idx, &value)
    }

    fn remove(&self, storage: &mut dyn Storage, _pk: PK, old_data: &T) {
        let idx = (self.indexer)(old_data);
        self.idx_map.remove(storage, idx);
    }
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct UniqueValue<PK, T> {
    pub value: T,
    pk: Vec<u8>,
    p: PhantomData<PK>,
}

impl<PK, T> UniqueValue<PK, T>
where
    PK: Key,
{
    pub fn new(pk: PK, value: T) -> Self {
        UniqueValue {
            pk: pk.joined_key(),
            value,
            p: PhantomData,
        }
    }

    pub fn key(&self) -> StdResult<PK::Output> {
        PK::from_slice(&self.pk)
    }

    pub fn key_value(&self) -> StdResult<(PK::Output, &T)> {
        Ok((PK::from_slice(&self.pk)?, &self.value))
    }

    pub fn key_value_deref(self) -> StdResult<(PK::Output, T)> {
        Ok((PK::from_slice(&self.pk)?, self.value))
    }
}
