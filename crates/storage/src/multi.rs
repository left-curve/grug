use {
    crate::{split_first_key, Borsh, Bound, Codec, Index, Key, Map, Prefix, Set},
    grug_types::{Empty, Order, Record, StdResult, Storage},
    std::marker::PhantomData,
};

// ------------------------------- multi index map ------------------------------

/// An indexer that allows multiple records in the primary map to have the same
/// index value.
pub struct MultiIndexMap<'a, PK, IK, T, C: Codec<T> = Borsh>
where
    PK: Key,
    IK: Key,
{
    indexer: fn(&PK, &T) -> IK,
    // The index set uses Borsh regardless of which codec the primary map uses.
    index_set: Set<'a, (IK, PK)>,
    primary_map: Map<'a, PK, T, C>,
}

impl<'a, PK, IK, T, C: Codec<T>> MultiIndexMap<'a, PK, IK, T, C>
where
    PK: Key,
    IK: Key,
{
    pub const fn new(
        indexer: fn(&PK, &T) -> IK,
        pk_namespace: &'a str,
        idx_namespace: &'static str,
    ) -> Self {
        MultiIndexMap {
            indexer,
            index_set: Set::new(idx_namespace),
            primary_map: Map::new(pk_namespace),
        }
    }

    /// Iterate records under a specific index value.
    ///
    /// E.g. If the index key is `(A, B)` and primary key is `(C, D)`, this
    /// allows you to give a value of `(A, B)` and iterate all `(C, D)` values.
    pub fn prefix(&self, idx: IK) -> IndexPrefix<IK, PK, PK, T, C> {
        IndexPrefix {
            prefix: Prefix::new(self.index_set.namespace, &idx.raw_keys()),
            primary_map: &self.primary_map,
            idx_ns: self.index_set.namespace.len(),
            phantom: PhantomData,
        }
    }

    /// Iterate records under a specific index prefix value.
    ///
    /// E.g. If the index key is `(A, B)` and primary key is `(C, D)`, this
    /// allows you to give a value of `A` and iterate all `(B, C, D)` values.
    pub fn sub_prefix(&self, idx: IK::Prefix) -> IndexPrefix<IK, PK, (IK::Suffix, PK), T, C> {
        IndexPrefix {
            prefix: Prefix::new(self.index_set.namespace, &idx.raw_keys()),
            primary_map: &self.primary_map,
            idx_ns: self.index_set.namespace.len(),
            phantom: PhantomData,
        }
    }

    pub fn range_raw<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<(IK, PK)>>,
        max: Option<Bound<(IK, PK)>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>, Vec<u8>)> + 'b> {
        let iter = self
            .index_set
            .range_raw(storage, min, max, order)
            .map(|ik_pk_raw| {
                let (ik_raw, pk_raw) = split_first_key(IK::KEY_ELEMS, &ik_pk_raw);
                // Load the data corresponding to the primary key from the
                // primary map.
                //
                // If the indexed map works correctly, the data should always exist,
                // so we can safely unwrap the `Option` here.
                let v_raw = self.primary_map.may_load_raw(storage, pk_raw).unwrap();
                (ik_raw, pk_raw.to_vec(), v_raw)
            });

        Box::new(iter)
    }

    pub fn range<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<(IK, PK)>>,
        max: Option<Bound<(IK, PK)>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(IK::Output, PK::Output, T)>> + 'b> {
        let iter = self
            .index_set
            .range_raw(storage, min, max, order)
            .map(|ik_pk_raw| {
                let (ik_raw, pk_raw) = split_first_key(IK::KEY_ELEMS, &ik_pk_raw);
                let ik = IK::deserialize(&ik_raw)?;
                let pk = PK::deserialize(pk_raw)?;
                let v_raw = self.primary_map.load_raw(storage, pk_raw)?;
                let v = C::decode(&v_raw)?;
                Ok((ik, pk, v))
            });

        Box::new(iter)
    }

    pub fn keys_raw<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<(IK, PK)>>,
        max: Option<Bound<(IK, PK)>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)> + 'b> {
        let iter = self
            .index_set
            .range_raw(storage, min, max, order)
            .map(|ik_pk_raw| {
                let (ik_raw, pk_raw) = split_first_key(IK::KEY_ELEMS, &ik_pk_raw);
                (ik_raw, pk_raw.to_vec())
            });

        Box::new(iter)
    }

    pub fn keys<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<(IK, PK)>>,
        max: Option<Bound<(IK, PK)>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(IK::Output, PK::Output)>> + 'b> {
        self.index_set.range(storage, min, max, order)
    }

    pub fn values_raw<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<(IK, PK)>>,
        max: Option<Bound<(IK, PK)>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        let iter = self
            .index_set
            .range_raw(storage, min, max, order)
            .map(|ik_pk_raw| {
                let (_, pk_raw) = split_first_key(IK::KEY_ELEMS, &ik_pk_raw);
                self.primary_map.may_load_raw(storage, pk_raw).unwrap()
            });

        Box::new(iter)
    }

    pub fn values<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<(IK, PK)>>,
        max: Option<Bound<(IK, PK)>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'b> {
        let iter = self
            .index_set
            .range_raw(storage, min, max, order)
            .map(|ik_pk_raw| {
                let (_, pk_raw) = split_first_key(IK::KEY_ELEMS, &ik_pk_raw);
                let v_raw = self.primary_map.may_load_raw(storage, pk_raw).unwrap();
                C::decode(&v_raw)
            });

        Box::new(iter)
    }
}

impl<'a, PK, IK, T, C: Codec<T>> Index<PK, T> for MultiIndexMap<'a, PK, IK, T, C>
where
    PK: Key,
    IK: Key,
{
    fn save(&self, storage: &mut dyn Storage, pk: PK, data: &T) -> StdResult<()> {
        let idx = (self.indexer)(&pk, data);
        self.index_set.insert(storage, (idx, pk))
    }

    fn remove(&self, storage: &mut dyn Storage, pk: PK, old_data: &T) {
        let idx = (self.indexer)(&pk, old_data);
        self.index_set.remove(storage, (idx, pk))
    }
}

// ---------------------------------- prefix -----------------------------------

pub struct IndexPrefix<'a, IK, PK, B, T, C: Codec<T>> {
    // The index set uses Borsh regardless of which codec the primary map uses.
    prefix: Prefix<B, Empty, Borsh>,
    primary_map: &'a Map<'a, PK, T, C>,
    idx_ns: usize,
    phantom: PhantomData<IK>,
}

impl<'a, IK, PK, B, T, C> IndexPrefix<'a, IK, PK, B, T, C>
where
    B: Key,
    C: Codec<T>,
{
    pub fn append(self, prefix: B::Prefix) -> IndexPrefix<'a, IK, PK, B::Suffix, T, C> {
        IndexPrefix {
            prefix: self.prefix.append(prefix),
            primary_map: self.primary_map,
            idx_ns: self.idx_ns,
            phantom: self.phantom,
        }
    }
}

impl<'a, IK, PK, B, T, C> IndexPrefix<'a, IK, PK, B, T, C>
where
    IK: Key,
    PK: Key,
    B: Key,
    C: Codec<T>,
{
    /// Iterate the raw primary keys and raw values under the given index value.
    pub fn range_raw<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<B>>,
        max: Option<Bound<B>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'b>
    where
        'a: 'b,
    {
        let iter = self
            .prefix
            .keys_raw_no_trimmer(storage, min, max, order)
            .map(|pk_raw| {
                let pk_raw = self.trim_key(&pk_raw);
                let v_raw = self.primary_map.may_load_raw(storage, pk_raw).unwrap();
                (pk_raw.to_vec(), v_raw)
            });

        Box::new(iter)
    }

    /// Iterate the primary keys and values under the given index value.
    pub fn range<'b>(
        &'b self,
        storage: &'b dyn Storage,
        min: Option<Bound<B>>,
        max: Option<Bound<B>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(PK::Output, T)>> + 'b>
    where
        'a: 'b,
    {
        let iter = self
            .prefix
            .keys_raw_no_trimmer(storage, min, max, order)
            .map(|pk_raw| {
                let pk_raw = self.trim_key(&pk_raw);
                let pk = PK::deserialize(pk_raw)?;
                let v_raw = self.primary_map.load_raw(storage, pk_raw)?;
                let v = C::decode(&v_raw)?;
                Ok((pk, v))
            });

        Box::new(iter)
    }

    /// Iterate the raw primary keys under the given index value.
    pub fn keys_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<B>>,
        max: Option<Bound<B>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.prefix.keys_raw(storage, min, max, order)
    }

    /// Iterate the primary keys under the given index value.
    pub fn keys<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<B>>,
        max: Option<Bound<B>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<B::Output>> + 'b> {
        self.prefix.keys(storage, min, max, order)
    }

    /// Iterate the raw values under the given index value.
    pub fn values_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<B>>,
        max: Option<Bound<B>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b>
    where
        'a: 'b,
    {
        let iter = self
            .prefix
            .keys_raw(storage, min, max, order)
            .map(|pk_raw| self.primary_map.load_raw(storage, &pk_raw).unwrap());

        Box::new(iter)
    }

    /// Iterate the values under the given index value.
    pub fn values<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<B>>,
        max: Option<Bound<B>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'b>
    where
        'a: 'b,
    {
        let iter = self
            .prefix
            .keys_raw(storage, min, max, order)
            .map(|pk_raw| {
                let v_raw = self.primary_map.load_raw(storage, &pk_raw)?;
                C::decode(&v_raw)
            });

        Box::new(iter)
    }

    fn trim_key<'b>(&self, key: &'b [u8]) -> &'b [u8] {
        let mut key = &key[self.idx_ns + 2..];

        // We trim the IK::Suffix and PK::Prefix.
        for _ in 0..IK::KEY_ELEMS {
            let (len, rest) = key.split_at(2);
            let a = u16::from_be_bytes([len[0], len[1]]);
            key = &rest[a as usize..];
        }

        key
    }
}
