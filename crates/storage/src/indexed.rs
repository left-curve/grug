use {
    crate::{Borsh, Bound, Codec, Key, Map, Prefix, Set},
    grug_types::{Order, Record, StdError, StdResult, Storage},
    std::marker::PhantomData,
};

pub trait IndexList<K, T> {
    fn get_indexes(&self) -> Box<dyn Iterator<Item = &'_ dyn Index<K, T>> + '_>;
}

pub trait Index<K, T> {
    fn save(&self, store: &mut dyn Storage, pk: K, data: &T) -> StdResult<()>;

    fn remove(&self, store: &mut dyn Storage, pk: K, old_data: &T);
}

// -------------------------------- indexed map --------------------------------

pub struct IndexedMap<'a, K, T, I, C: Codec<T> = Borsh> {
    pk_namespace: &'a [u8],
    primary: Map<'a, K, T, C>,
    /// This is meant to be read directly to get the proper types, like:
    /// `map.idx.owner.items(...)`.
    pub idx: I,
}

impl<'a, K, T, I, C: Codec<T>> IndexedMap<'a, K, T, I, C>
where
    K: Key,
{
    pub const fn new(pk_namespace: &'static str, indexes: I) -> Self {
        IndexedMap {
            pk_namespace: pk_namespace.as_bytes(),
            primary: Map::new(pk_namespace),
            idx: indexes,
        }
    }

    fn no_prefix(&self) -> Prefix<K, T, C> {
        Prefix::new(self.pk_namespace, &[])
    }

    pub fn prefix(&self, prefix: K::Prefix) -> Prefix<K::Suffix, T, C> {
        Prefix::new(self.pk_namespace, &prefix.raw_keys())
    }

    pub fn is_empty(&self, storage: &dyn Storage) -> bool {
        self.no_prefix()
            .keys_raw(storage, None, None, Order::Ascending)
            .next()
            .is_none()
    }

    pub fn has(&self, storage: &dyn Storage, k: K) -> bool {
        self.primary.has(storage, k)
    }

    pub fn may_load(&self, storage: &dyn Storage, key: K) -> StdResult<Option<T>> {
        self.primary.may_load(storage, key)
    }

    pub fn load(&self, storage: &dyn Storage, key: K) -> StdResult<T> {
        self.primary.load(storage, key)
    }

    pub fn range_raw<'b>(
        &self,
        store: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'b>
    where
        T: 'b,
    {
        self.no_prefix().range_raw(store, min, max, order)
    }

    pub fn range<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(K::Output, T)>> + 'b> {
        self.no_prefix().range(storage, min, max, order)
    }

    pub fn keys_raw<'b>(
        &self,
        store: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b>
    where
        T: 'b,
    {
        self.no_prefix().keys_raw(store, min, max, order)
    }

    pub fn keys<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<K::Output>> + 'b> {
        self.no_prefix().keys(storage, min, max, order)
    }

    pub fn values_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.no_prefix().values_raw(storage, min, max, order)
    }

    pub fn values<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'b> {
        self.no_prefix().values(storage, min, max, order)
    }

    pub fn clear(&self, storage: &mut dyn Storage, min: Option<Bound<K>>, max: Option<Bound<K>>) {
        self.no_prefix().clear(storage, min, max)
    }
}

impl<'a, K, T, I, C: Codec<T>> IndexedMap<'a, K, T, I, C>
where
    K: Key + Clone,
    I: IndexList<K, T>,
{
    pub fn save(&'a self, storage: &mut dyn Storage, key: K, data: &T) -> StdResult<()> {
        let old_data = self.may_load(storage, key.clone())?;
        self.replace(storage, key, Some(data), old_data.as_ref())
    }

    pub fn remove(&'a self, storage: &mut dyn Storage, key: K) -> StdResult<()> {
        let old_data = self.may_load(storage, key.clone())?;
        self.replace(storage, key, None, old_data.as_ref())
    }

    fn replace(
        &'a self,
        storage: &mut dyn Storage,
        key: K,
        data: Option<&T>,
        old_data: Option<&T>,
    ) -> StdResult<()> {
        // If old data exists, its index is to be deleted.
        if let Some(old) = old_data {
            for index in self.idx.get_indexes() {
                index.remove(storage, key.clone(), old);
            }
        }

        // Write new data to the primary store, and write its indexes.
        if let Some(updated) = data {
            for index in self.idx.get_indexes() {
                index.save(storage, key.clone(), updated)?;
            }
            self.primary.save(storage, key, updated)?;
        } else {
            self.primary.remove(storage, key);
        }

        Ok(())
    }
}

impl<'a, K, T, I, C: Codec<T>> IndexedMap<'a, K, T, I, C>
where
    K: Key + Clone,
    T: Clone,
    I: IndexList<K, T>,
{
    pub fn update<A, Err>(
        &'a self,
        storage: &mut dyn Storage,
        key: K,
        action: A,
    ) -> Result<Option<T>, Err>
    where
        A: FnOnce(Option<T>) -> Result<Option<T>, Err>,
        Err: From<StdError>,
    {
        let old_data = self.may_load(storage, key.clone())?;
        let new_data = action(old_data.clone())?;

        self.replace(storage, key, new_data.as_ref(), old_data.as_ref())?;

        Ok(new_data)
    }
}

// -------------------------------- indexed set --------------------------------

pub struct IndexedSet<'a, K, I> {
    _pk_namespace: &'a [u8],
    _primary: Set<'a, K>,
    /// This is meant to be read directly to get the proper types, like:
    /// `set.idx.owner.items(...)`.
    pub idx: PhantomData<I>,
}

impl<'a, K, I> IndexedSet<'a, K, I> {
    pub const fn new(pk_namespace: &'static str) -> Self {
        IndexedSet {
            _pk_namespace: pk_namespace.as_bytes(),
            _primary: Set::new(pk_namespace),
            idx: PhantomData,
        }
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{Bound, Index, IndexList, IndexedMap, MultiIndexMap, UniqueIndexMap},
        borsh::{BorshDeserialize, BorshSerialize},
        grug_types::{MockStorage, Order, StdResult},
    };

    const FOOS: IndexedMap<(u64, u64), Foo, FooIndexes> = IndexedMap::new("foo", FooIndexes {
        name: MultiIndexMap::new(|_, data| data.name.clone(), "foo", "foo__name"),
        name_surname: MultiIndexMap::new(
            |_, data| (data.name.clone(), data.surname.clone()),
            "foo",
            "foo__name_surname",
        ),
        id: UniqueIndexMap::new(|data| data.id, "foo__id"),
    });

    #[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
    struct Foo {
        pub name: String,
        pub surname: String,
        pub id: u32,
    }

    impl Foo {
        pub fn new(name: &str, surname: &str, id: u32) -> Self {
            Foo {
                name: name.to_string(),
                surname: surname.to_string(),
                id,
            }
        }
    }

    struct FooIndexes<'a> {
        pub name: MultiIndexMap<'a, (u64, u64), String, Foo>,
        pub name_surname: MultiIndexMap<'a, (u64, u64), (String, String), Foo>,
        pub id: UniqueIndexMap<'a, u32, Foo>,
    }

    impl<'a> IndexList<(u64, u64), Foo> for FooIndexes<'a> {
        fn get_indexes(&self) -> Box<dyn Iterator<Item = &'_ dyn Index<(u64, u64), Foo>> + '_> {
            let v: Vec<&dyn Index<(u64, u64), Foo>> =
                vec![&self.name, &self.id, &self.name_surname];
            Box::new(v.into_iter())
        }
    }

    fn setup_test() -> MockStorage {
        let mut storage = MockStorage::new();

        for (key, name, surname, id) in [
            ((0, 1), "bar", "s_bar", 101),
            ((0, 2), "bar", "s_bar", 102),
            ((1, 1), "bar", "s_bar", 103),
            ((1, 2), "bar", "s_fooes", 104),
            ((1, 3), "foo", "s_foo", 105),
        ] {
            FOOS.save(&mut storage, key, &Foo::new(name, surname, id))
                .unwrap();
        }

        storage
    }

    #[test]
    fn unique_index_works() {
        let mut storage = setup_test();

        // Load a single data by the index.
        {
            let val = FOOS.idx.id.load(&storage, 104).unwrap();
            assert_eq!(val, Foo::new("bar", "s_fooes", 104));
        }

        // Try to save a data with duplicate index; should fail.
        {
            FOOS.save(&mut storage, (5, 5), &Foo::new("bar", "s_fooes", 104))
                .unwrap_err();
        }

        // Iterate index values and data.
        {
            let val = FOOS
                .idx
                .id
                .range(&storage, None, None, Order::Ascending)
                .map(|val| val.unwrap())
                .collect::<Vec<_>>();

            assert_eq!(val, vec![
                (101, Foo::new("bar", "s_bar", 101)),
                (102, Foo::new("bar", "s_bar", 102)),
                (103, Foo::new("bar", "s_bar", 103)),
                (104, Foo::new("bar", "s_fooes", 104)),
                (105, Foo::new("foo", "s_foo", 105))
            ]);
        }
    }

    /// Multi index, where the index key is a singleton.
    #[test]
    fn multi_index_singleton_works() {
        let storage = setup_test();

        // Iterate all index values and records.
        {
            let val = FOOS
                .idx
                .name
                .range(&storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![
                ("bar".to_string(), (0, 1), Foo::new("bar", "s_bar", 101)),
                ("bar".to_string(), (0, 2), Foo::new("bar", "s_bar", 102)),
                ("bar".to_string(), (1, 1), Foo::new("bar", "s_bar", 103)),
                ("bar".to_string(), (1, 2), Foo::new("bar", "s_fooes", 104)),
                ("foo".to_string(), (1, 3), Foo::new("foo", "s_foo", 105)),
            ]);
        }

        // Given a specific index value, iterate records corresponding to it.
        //
        // In this test case, we find all foos whose name is "bar".
        {
            let val = FOOS
                .idx
                .name
                .prefix("bar".to_string())
                .range(&storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![
                ((0, 1), Foo::new("bar", "s_bar", 101)),
                ((0, 2), Foo::new("bar", "s_bar", 102)),
                ((1, 1), Foo::new("bar", "s_bar", 103)),
                ((1, 2), Foo::new("bar", "s_fooes", 104)),
            ]);
        }
    }

    /// Multi index, where the index key is a tuple.
    ///
    /// In this case,
    ///
    /// - index key is `name_surname` of `(String, String)` type;
    /// - primary key is of `(u64, u64)` type;
    /// - data is of `Foo` type.
    ///
    /// The index set is therefore a `Set<((String, String), (u64, u64))>`.
    ///
    /// Let's denote the index key as `(A, B)` and the primary key as `(C, D)`.
    #[test]
    fn multi_index_tuple_works() {
        let storage = setup_test();

        // Given (A, B), iterate (C, D), without bounds.
        //
        // In this test case, we find all foos whose name is "bar" and last name
        // is "s_bar".
        {
            let val = FOOS
                .idx
                .name_surname
                .prefix(("bar".to_string(), "s_bar".to_string()))
                .range(&storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![
                ((0, 1), Foo::new("bar", "s_bar", 101)),
                ((0, 2), Foo::new("bar", "s_bar", 102)),
                ((1, 1), Foo::new("bar", "s_bar", 103)),
            ]);
        }

        // Given (A, B), iterate (C, D), with bounds.
        //
        // Same as the previous test case, but the with bounds for (C, D).
        {
            let val = FOOS
                .idx
                .name_surname
                .prefix(("bar".to_string(), "s_bar".to_string()))
                .range(
                    &storage,
                    Some(Bound::Inclusive((0, 2))),
                    None,
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![
                ((0, 2), Foo::new("bar", "s_bar", 102)),
                ((1, 1), Foo::new("bar", "s_bar", 103)),
            ]);
        }

        // Given A, iterate (B, C, D), without bounds.
        //
        // In this test case, we find all foos whose name is "bar".
        {
            let val = FOOS
                .idx
                .name_surname
                .sub_prefix("bar".to_string())
                .range(&storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![
                ((0, 1), Foo::new("bar", "s_bar", 101)),
                ((0, 2), Foo::new("bar", "s_bar", 102)),
                ((1, 1), Foo::new("bar", "s_bar", 103)),
                ((1, 2), Foo::new("bar", "s_fooes", 104)),
            ]);
        }

        // Given A, iterate (B, C, D), with bounds.
        //
        // Same as the previous test case, but (B, C, D) must be greater than
        // ("bar", 0, 1).
        {
            let val = FOOS
                .idx
                .name_surname
                .sub_prefix("bar".to_string())
                .range(
                    &storage,
                    Some(Bound::Exclusive(("s_bar".to_string(), (0, 1)))),
                    None,
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![
                ((0, 2), Foo::new("bar", "s_bar", 102)),
                ((1, 1), Foo::new("bar", "s_bar", 103)),
                ((1, 2), Foo::new("bar", "s_fooes", 104)),
            ]);
        }

        // Given (A, B, C), iterate D, without bounds.
        //
        // In this test case, we find all foos whose name is "bar" and surname
        // is "s_bar" and the first number in the primary key is 0.
        {
            let val = FOOS
                .idx
                .name_surname
                .prefix(("bar".to_string(), "s_bar".to_string()))
                .append(0)
                .range(&storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![
                ((0, 1), Foo::new("bar", "s_bar", 101)),
                ((0, 2), Foo::new("bar", "s_bar", 102)),
            ]);
        }

        // Given (A, B, C), iterate D, with bounds.
        //
        // Same with the previous test case, but D must be greater than 1.
        {
            let val = FOOS
                .idx
                .name_surname
                .prefix(("bar".to_string(), "s_bar".to_string()))
                .append(0)
                .range(&storage, Some(Bound::Exclusive(1)), None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(val, vec![((0, 2), Foo::new("bar", "s_bar", 102)),]);
        }
    }
}
