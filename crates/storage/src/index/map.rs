use {
    crate::{Borsh, Bound, Codec, Key, Map, Prefix, PrefixBound},
    grug_types::{Order, Record, StdError, StdResult, Storage},
};

pub trait IndexList<'a, K, T> {
    fn get_indexes(&'a self) -> Box<dyn Iterator<Item = &'a dyn Index<K, T>> + '_>;
}

pub trait Index<K, T> {
    fn save(&self, store: &mut dyn Storage, pk: K, data: &T) -> StdResult<()>;

    fn remove(&self, store: &mut dyn Storage, pk: K, old_data: &T);
}

pub struct IndexedMap<'a, K, T, I, C = Borsh>
where
    C: Codec<T>,
{
    primary: Map<'a, K, T, C>,
    /// This is meant to be read directly to get the proper types, like:
    /// `map.idx.owner.items(...)`.
    pub idx: I,
}

impl<'a, K, T, I, C> IndexedMap<'a, K, T, I, C>
where
    K: Key,
    C: Codec<T>,
{
    pub const fn new(pk_namespace: &'static str, indexes: I) -> Self {
        IndexedMap {
            primary: Map::new(pk_namespace),
            idx: indexes,
        }
    }

    pub fn prefix(&self, prefix: K::Prefix) -> Prefix<K::Suffix, T, C> {
        self.primary.prefix(prefix)
    }

    pub fn is_empty(&self, storage: &dyn Storage) -> bool {
        self.primary.is_empty(storage)
    }

    // ---------------------- methods for single entries -----------------------

    pub fn has_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> bool {
        self.primary.has_raw(storage, key_raw)
    }

    pub fn has(&self, storage: &dyn Storage, k: K) -> bool {
        self.primary.has(storage, k)
    }

    pub fn may_load_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> Option<Vec<u8>> {
        self.primary.may_load_raw(storage, key_raw)
    }

    pub fn may_load(&self, storage: &dyn Storage, key: K) -> StdResult<Option<T>> {
        self.primary.may_load(storage, key)
    }

    pub fn load_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> StdResult<Vec<u8>> {
        self.primary.load_raw(storage, key_raw)
    }

    pub fn load(&self, storage: &dyn Storage, key: K) -> StdResult<T> {
        self.primary.load(storage, key)
    }

    // -------------------- iteration methods (full bound) ---------------------

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
        self.primary.range_raw(store, min, max, order)
    }

    pub fn range<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(K::Output, T)>> + 'b> {
        self.primary.range(storage, min, max, order)
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
        self.primary.keys_raw(store, min, max, order)
    }

    pub fn keys<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<K::Output>> + 'b> {
        self.primary.keys(storage, min, max, order)
    }

    pub fn values_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.primary.values_raw(storage, min, max, order)
    }

    pub fn values<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'b> {
        self.primary.values(storage, min, max, order)
    }

    pub fn clear(&self, storage: &mut dyn Storage, min: Option<Bound<K>>, max: Option<Bound<K>>) {
        self.primary.clear(storage, min, max)
    }

    // ------------------- iteration methods (prefix bound) --------------------

    pub fn prefix_range_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'b> {
        self.primary.prefix_range_raw(storage, min, max, order)
    }

    pub fn prefix_range<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(K::Output, T)>> + 'b> {
        self.primary.prefix_range(storage, min, max, order)
    }

    pub fn prefix_keys_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.primary.prefix_keys_raw(storage, min, max, order)
    }

    pub fn prefix_keys<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<K::Output>> + 'b> {
        self.primary.prefix_keys(storage, min, max, order)
    }

    pub fn prefix_values_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.primary.prefix_values_raw(storage, min, max, order)
    }

    pub fn prefix_values<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'b> {
        self.primary.prefix_values(storage, min, max, order)
    }

    pub fn prefix_clear(
        &self,
        storage: &mut dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
    ) {
        self.primary.prefix_clear(storage, min, max)
    }
}

impl<'a, K, T, I, C> IndexedMap<'a, K, T, I, C>
where
    K: Key + Clone,
    I: IndexList<'a, K, T>,
    C: Codec<T>,
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

impl<'a, K, T, I, C> IndexedMap<'a, K, T, I, C>
where
    K: Key + Clone,
    T: Clone,
    I: IndexList<'a, K, T>,
    C: Codec<T>,
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

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{Bound, Index, IndexList, IndexedMap, MultiIndex, UniqueIndex},
        borsh::{BorshDeserialize, BorshSerialize},
        grug_types::{MockStorage, Order, StdResult},
    };

    const FOOS: IndexedMap<(u64, u64), Foo, FooIndexes> = IndexedMap::new("foo", FooIndexes {
        name: MultiIndex::new(|_, data| data.name.clone(), "foo", "foo__name"),
        name_surname: MultiIndex::new(
            |_, data| (data.name.clone(), data.surname.clone()),
            "foo",
            "foo__name_surname",
        ),
        id: UniqueIndex::new(|data| data.id, "foo__id"),
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
        pub name: MultiIndex<'a, (u64, u64), String, Foo>,
        pub name_surname: MultiIndex<'a, (u64, u64), (String, String), Foo>,
        pub id: UniqueIndex<'a, (u64, u64), u32, Foo>,
    }

    impl<'a> IndexList<'a, (u64, u64), Foo> for FooIndexes<'a> {
        fn get_indexes(&'a self) -> Box<dyn Iterator<Item = &'a dyn Index<(u64, u64), Foo>> + 'a> {
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
            assert_eq!(
                val.key_value_deref().unwrap(),
                ((1, 2), Foo::new("bar", "s_fooes", 104))
            );
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
                .map(|val| {
                    let val = val.unwrap();
                    (val.0, val.1.value)
                })
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

#[cfg(test)]
mod cosmwasm_tests {
    use {
        super::{Index, IndexList, IndexedMap},
        crate::{Borsh, Bound, Key, MultiIndex, PrefixBound, UniqueIndex, UniqueValue},
        borsh::{BorshDeserialize, BorshSerialize},
        grug_types::{from_borsh_slice, to_borsh_vec, MockStorage, Order, StdResult},
    };

    #[derive(BorshSerialize, BorshDeserialize, PartialEq, Debug, Clone)]
    struct Data {
        pub name: String,
        pub last_name: String,
        pub age: u32,
    }

    struct DataIndexes<'a> {
        // Last type parameters are for signaling pk deserialization
        pub name: MultiIndex<'a, &'a str, String, Data>,
        pub age: UniqueIndex<'a, &'a str, u32, Data>,
        pub name_lastname: UniqueIndex<'a, &'a str, (Vec<u8>, Vec<u8>), Data>,
    }

    // Future Note: this can likely be macro-derived
    impl<'a> IndexList<'a, &'a str, Data> for DataIndexes<'a> {
        fn get_indexes(&'a self) -> Box<dyn Iterator<Item = &'a dyn Index<&str, Data>> + '_> {
            let v: Vec<&dyn Index<&str, Data>> = vec![&self.name, &self.age, &self.name_lastname];
            Box::new(v.into_iter())
        }
    }

    // For composite multi index tests
    struct DataCompositeMultiIndex<'a, PK>
    where
        PK: Key,
    {
        // Last type parameter is for signaling pk deserialization
        pub name_age: MultiIndex<'a, PK, (Vec<u8>, u32), Data>,
    }

    // Future Note: this can likely be macro-derived
    impl<'a, PK> IndexList<'a, PK, Data> for DataCompositeMultiIndex<'a, PK>
    where
        PK: Key,
    {
        fn get_indexes(&'_ self) -> Box<dyn Iterator<Item = &'_ dyn Index<PK, Data>> + '_> {
            let v: Vec<&dyn Index<PK, Data>> = vec![&self.name_age];
            Box::new(v.into_iter())
        }
    }

    const DATA: IndexedMap<&str, Data, DataIndexes> = IndexedMap::new("data", DataIndexes {
        name: MultiIndex::new(|_pk, d| d.name.to_string(), "data", "data__name"),
        age: UniqueIndex::new(|d| d.age, "data__age"),
        name_lastname: UniqueIndex::new(
            |d| index_string_tuple(&d.name, &d.last_name),
            "data__name_lastname",
        ),
    });

    fn index_string(data: &str) -> Vec<u8> {
        data.as_bytes().to_vec()
    }

    fn index_tuple(name: &str, age: u32) -> (Vec<u8>, u32) {
        (index_string(name), age)
    }

    fn index_string_tuple(data1: &str, data2: &str) -> (Vec<u8>, Vec<u8>) {
        (index_string(data1), index_string(data2))
    }

    fn save_data<'a>(store: &mut MockStorage) -> (Vec<&'a str>, Vec<Data>) {
        let mut pks = vec![];
        let mut datas = vec![];
        let data = Data {
            name: "Maria".to_string(),
            last_name: "Doe".to_string(),
            age: 42,
        };
        let pk = "1";
        DATA.save(store, pk, &data).unwrap();
        pks.push(pk);
        datas.push(data);

        // same name (multi-index), different last name, different age => ok
        let data = Data {
            name: "Maria".to_string(),
            last_name: "Williams".to_string(),
            age: 23,
        };
        let pk = "2";
        DATA.save(store, pk, &data).unwrap();
        pks.push(pk);
        datas.push(data);

        // different name, different last name, different age => ok
        let data = Data {
            name: "John".to_string(),
            last_name: "Wayne".to_string(),
            age: 32,
        };
        let pk = "3";
        DATA.save(store, pk, &data).unwrap();
        pks.push(pk);
        datas.push(data);

        let data = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Rodriguez".to_string(),
            age: 12,
        };
        let pk = "4";
        DATA.save(store, pk, &data).unwrap();
        pks.push(pk);
        datas.push(data);

        let data = Data {
            name: "Marta".to_string(),
            last_name: "After".to_string(),
            age: 90,
        };
        let pk = "5";
        DATA.save(store, pk, &data).unwrap();
        pks.push(pk);
        datas.push(data);

        (pks, datas)
    }

    #[test]
    fn store_and_load_by_index() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);
        let pk = pks[0];
        let data = &datas[0];

        // load it properly
        let loaded = DATA.load(&store, pk).unwrap();
        assert_eq!(*data, loaded);

        let count = DATA
            .idx
            .name
            .prefix("Maria".to_string())
            .range_raw(&store, None, None, Order::Ascending)
            .count();
        assert_eq!(2, count);

        // load it by secondary index
        let marias: Vec<_> = DATA
            .idx
            .name
            .prefix("Maria".to_string())
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(2, marias.len());
        let (k, v) = &marias[0];
        assert_eq!(pk, String::from_slice(k).unwrap());
        assert_eq!(data, &from_borsh_slice::<_, Data>(v).unwrap());

        // other index doesn't match (1 byte after)
        let count = DATA
            .idx
            .name
            .prefix("Marib".to_string())
            .range_raw(&store, None, None, Order::Ascending)
            .count();
        assert_eq!(0, count);

        // other index doesn't match (1 byte before)
        let count = DATA
            .idx
            .name
            .prefix("Mari`".to_string())
            .range_raw(&store, None, None, Order::Ascending)
            .count();
        assert_eq!(0, count);

        // other index doesn't match (longer)
        let count = DATA
            .idx
            .name
            .prefix("Maria5".to_string())
            .range_raw(&store, None, None, Order::Ascending)
            .count();
        assert_eq!(0, count);

        // In a MultiIndex, the index key is composed by the index and the primary key.
        // Primary key may be empty (so that to iterate over all elements that match just the index)
        let key = ("Maria".to_string(), "");
        // Iterate using an inclusive bound over the key
        let marias = DATA
            .idx
            .name
            .range_raw(&store, Some(Bound::inclusive(key)), None, Order::Ascending)
            .collect::<Vec<_>>();
        // gets from the first "Maria" until the end
        assert_eq!(4, marias.len());

        // Build key including a non-empty pk
        let key = ("Maria".to_string(), "1");
        // Iterate using a (exclusive) bound over the key.
        // (Useful for pagination / continuation contexts).
        let count = DATA
            .idx
            .name
            .range_raw(&store, Some(Bound::exclusive(key)), None, Order::Ascending)
            .count();
        // gets from the 2nd "Maria" until the end
        assert_eq!(3, count);

        // index_key() over UniqueIndex works.
        let age_key = 23u32;
        // Iterate using a (inclusive) bound over the key.
        let count = DATA
            .idx
            .age
            .range_raw(
                &store,
                Some(Bound::inclusive(age_key)),
                None,
                Order::Ascending,
            )
            .count();
        // gets all the greater than or equal to 23 years old people
        assert_eq!(4, count);

        // match on proper age
        let proper = 42u32;
        let aged = DATA.idx.age.load(&store, proper).unwrap();

        let (k, v) = aged.key_value().unwrap();
        assert_eq!(pk, k);
        assert_eq!(data, v);

        // no match on wrong age
        let too_old = 43u32;
        DATA.idx.age.load(&store, too_old).unwrap_err();
    }

    #[test]
    fn existence() {
        let mut store = MockStorage::new();
        let (pks, _) = save_data(&mut store);

        assert!(DATA.has(&store, pks[0]));
        assert!(!DATA.has(&store, "6"));
    }

    #[test]
    fn range_raw_simple_key_by_multi_index() {
        let mut store = MockStorage::new();

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk = "5627";
        DATA.save(&mut store, pk, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk = "5628";
        DATA.save(&mut store, pk, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Williams".to_string(),
            age: 24,
        };
        let pk = "5629";
        DATA.save(&mut store, pk, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 12,
        };
        let pk = "5630";
        DATA.save(&mut store, pk, &data4).unwrap();

        let marias: Vec<_> = DATA
            .idx
            .name
            .prefix("Maria".to_string())
            .range_raw(&store, None, None, Order::Descending)
            .collect();
        let count = marias.len();
        assert_eq!(2, count);

        // Pks, sorted by (descending) pk
        assert_eq!(marias[0].0, b"5629");
        assert_eq!(marias[1].0, b"5627");
        // Data is correct
        assert_eq!(marias[0].1, to_borsh_vec(&data3).unwrap());
        assert_eq!(marias[1].1, to_borsh_vec(&data1).unwrap());
    }

    #[test]
    fn range_simple_key_by_multi_index() {
        let mut store = MockStorage::new();

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk = "5627";
        DATA.save(&mut store, pk, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk = "5628";
        DATA.save(&mut store, pk, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Williams".to_string(),
            age: 24,
        };
        let pk = "5629";
        DATA.save(&mut store, pk, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 12,
        };
        let pk = "5630";
        DATA.save(&mut store, pk, &data4).unwrap();

        let marias: Vec<_> = DATA
            .idx
            .name
            .prefix("Maria".to_string())
            .range(&store, None, None, Order::Descending)
            .collect::<StdResult<_>>()
            .unwrap();
        let count = marias.len();
        assert_eq!(2, count);

        // Pks, sorted by (descending) pk
        assert_eq!(marias[0].0, "5629");
        assert_eq!(marias[1].0, "5627");
        // Data is correct
        assert_eq!(marias[0].1, data3);
        assert_eq!(marias[1].1, data1);
    }

    #[test]
    fn range_raw_composite_key_by_multi_index() {
        let mut store = MockStorage::new();

        let indexes = DataCompositeMultiIndex {
            name_age: MultiIndex::new(
                |_pk, d| index_tuple(&d.name, d.age),
                "data",
                "data__name_age",
            ),
        };
        let map: IndexedMap<&[u8], Data, DataCompositeMultiIndex<&[u8]>> =
            IndexedMap::new("data", indexes);

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk1: &[u8] = b"5627";
        map.save(&mut store, pk1, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk2: &[u8] = b"5628";
        map.save(&mut store, pk2, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Young".to_string(),
            age: 24,
        };
        let pk3: &[u8] = b"5629";
        map.save(&mut store, pk3, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 43,
        };
        let pk4: &[u8] = b"5630";
        map.save(&mut store, pk4, &data4).unwrap();

        let marias: Vec<_> = map
            .idx
            .name_age
            .sub_prefix(b"Maria".to_vec())
            .range_raw(&store, None, None, Order::Descending)
            .collect();
        let count = marias.len();
        assert_eq!(2, count);

        // Pks, sorted by (descending) age
        assert_eq!(pk1, marias[0].0);
        assert_eq!(pk3, marias[1].0);

        // Data
        assert_eq!(to_borsh_vec(&data1).unwrap(), marias[0].1);
        assert_eq!(to_borsh_vec(&data3).unwrap(), marias[1].1);
    }

    #[test]
    fn range_composite_key_by_multi_index() {
        let mut store = MockStorage::new();

        let indexes = DataCompositeMultiIndex {
            name_age: MultiIndex::new(
                |_pk, d| index_tuple(&d.name, d.age),
                "data",
                "data__name_age",
            ),
        };
        let map: IndexedMap<&[u8], Data, DataCompositeMultiIndex<&[u8]>, Borsh> =
            IndexedMap::new("data", indexes);

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk1 = b"5627";
        map.save(&mut store, pk1, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk2 = b"5628";
        map.save(&mut store, pk2, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Young".to_string(),
            age: 24,
        };
        let pk3 = b"5629";
        map.save(&mut store, pk3, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 43,
        };
        let pk4 = b"5630";
        map.save(&mut store, pk4, &data4).unwrap();

        let marias: Vec<_> = map
            .idx
            .name_age
            .sub_prefix(b"Maria".to_vec())
            .range(&store, None, None, Order::Descending)
            .collect::<StdResult<_>>()
            .unwrap();
        let count = marias.len();
        assert_eq!(2, count);

        // Pks, sorted by (descending) age
        assert_eq!(pk1.to_vec(), marias[0].0);
        assert_eq!(pk3.to_vec(), marias[1].0);

        // Data
        assert_eq!(data1, marias[0].1);
        assert_eq!(data3, marias[1].1);
    }

    #[test]
    fn unique_index_enforced() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);

        // different name, different last name, same age => error
        let data5 = Data {
            name: "Marcel".to_string(),
            last_name: "Laurens".to_string(),
            age: 42,
        };
        let pk5 = "4";

        // enforce this returns some error
        DATA.save(&mut store, pk5, &data5).unwrap_err();

        // query by unique key
        // match on proper age
        let age42 = 42u32;
        let (k, v) = DATA
            .idx
            .age
            .load(&store, age42)
            .unwrap()
            .key_value_deref()
            .unwrap();
        assert_eq!(k, pks[0]);
        assert_eq!(v.name, datas[0].name);
        assert_eq!(v.age, datas[0].age);

        // match on other age
        let age23 = 23u32;
        let (k, v) = DATA
            .idx
            .age
            .load(&store, age23)
            .unwrap()
            .key_value_deref()
            .unwrap();
        assert_eq!(k, pks[1]);
        assert_eq!(v.name, datas[1].name);
        assert_eq!(v.age, datas[1].age);

        // if we delete the first one, we can add the blocked one
        DATA.remove(&mut store, pks[0]).unwrap();
        DATA.save(&mut store, pk5, &data5).unwrap();
        // now 42 is the new owner
        let (k, v) = DATA
            .idx
            .age
            .load(&store, age42)
            .unwrap()
            .key_value_deref()
            .unwrap();
        assert_eq!(k, pk5);
        assert_eq!(v.name, data5.name);
        assert_eq!(v.age, data5.age);
    }

    #[test]
    fn unique_index_enforced_composite_key() {
        let mut store = MockStorage::new();

        // save data
        save_data(&mut store);

        // same name, same lastname => error
        let data5 = Data {
            name: "Maria".to_string(),
            last_name: "Doe".to_string(),
            age: 24,
        };
        let pk5 = "5";
        // enforce this returns some error
        DATA.save(&mut store, pk5, &data5).unwrap_err();
    }

    #[test]
    fn remove_and_update_reflected_on_indexes() {
        let mut store = MockStorage::new();

        let name_count = |store: &MockStorage, name: &str| -> usize {
            DATA.idx
                .name
                .prefix(name.to_string())
                .keys_raw(store, None, None, Order::Ascending)
                .count()
        };

        // save data
        let (pks, _) = save_data(&mut store);

        // find 2 Marias, 1 John, and no Mary
        assert_eq!(name_count(&store, "Maria"), 2);
        assert_eq!(name_count(&store, "John"), 1);
        assert_eq!(name_count(&store, "Maria Luisa"), 1);
        assert_eq!(name_count(&store, "Mary"), 0);

        // remove maria 2
        DATA.remove(&mut store, pks[1]).unwrap();

        // change john to mary
        DATA.update(&mut store, pks[2], |d| -> StdResult<_> {
            let mut x = d.unwrap();
            assert_eq!(&x.name, "John");
            x.name = "Mary".to_string();
            Ok(Some(x))
        })
        .unwrap();

        // find 1 maria, 1 maria luisa, no john, and 1 mary
        assert_eq!(name_count(&store, "Maria"), 1);
        assert_eq!(name_count(&store, "Maria Luisa"), 1);
        assert_eq!(name_count(&store, "John"), 0);
        assert_eq!(name_count(&store, "Mary"), 1);
    }

    #[test]
    fn range_raw_simple_key_by_unique_index() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);

        let ages: Vec<_> = DATA
            .idx
            .age
            .range_raw(&store, None, None, Order::Ascending)
            .map(|(k, v)| {
                (
                    k,
                    from_borsh_slice::<_, UniqueValue<&str, Data>>(&v).unwrap(),
                )
            })
            .collect();

        let count = ages.len();
        assert_eq!(5, count);

        // The ik, sorted by age ascending
        assert_eq!(datas[3].age.to_be_bytes(), ages[0].0.as_slice()); // 12
        assert_eq!(datas[1].age.to_be_bytes(), ages[1].0.as_slice()); // 23
        assert_eq!(datas[2].age.to_be_bytes(), ages[2].0.as_slice()); // 32
        assert_eq!(datas[0].age.to_be_bytes(), ages[3].0.as_slice()); // 42
        assert_eq!(datas[4].age.to_be_bytes(), ages[4].0.as_slice()); // 90

        // The pks, sorted by age ascending
        assert_eq!(pks[3], ages[0].1.key().unwrap()); // 12
        assert_eq!(pks[1], ages[1].1.key().unwrap()); // 23
        assert_eq!(pks[2], ages[2].1.key().unwrap()); // 32
        assert_eq!(pks[0], ages[3].1.key().unwrap()); // 42
        assert_eq!(pks[4], ages[4].1.key().unwrap()); // 90

        // The associated data
        assert_eq!(datas[3], ages[0].1.value);
        assert_eq!(datas[1], ages[1].1.value);
        assert_eq!(datas[2], ages[2].1.value);
        assert_eq!(datas[0], ages[3].1.value);
        assert_eq!(datas[4], ages[4].1.value);
    }

    #[test]
    fn range_simple_key_by_unique_index() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);

        let res: StdResult<Vec<_>> = DATA
            .idx
            .age
            .values(&store, None, None, Order::Ascending)
            .collect();
        let ages = res.unwrap();

        let count = ages.len();
        assert_eq!(5, count);

        // The pks, sorted by age ascending
        assert_eq!(pks[3], ages[0].key().unwrap());
        assert_eq!(pks[1], ages[1].key().unwrap());
        assert_eq!(pks[2], ages[2].key().unwrap());
        assert_eq!(pks[0], ages[3].key().unwrap());
        assert_eq!(pks[4], ages[4].key().unwrap());

        // The associated data
        assert_eq!(datas[3], ages[0].value);
        assert_eq!(datas[1], ages[1].value);
        assert_eq!(datas[2], ages[2].value);
        assert_eq!(datas[0], ages[3].value);
        assert_eq!(datas[4], ages[4].value);
    }

    #[test]
    fn range_raw_composite_key_by_unique_index() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);

        let marias = DATA
            .idx
            .name_lastname
            .prefix(b"Maria".to_vec())
            .range_raw(&store, None, None, Order::Ascending)
            .map(|(k, v)| {
                (
                    k,
                    from_borsh_slice::<_, UniqueValue<&str, Data>>(&v).unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // Only two people are called "Maria"
        let count = marias.len();
        assert_eq!(2, count);

        // The ik::suffix
        assert_eq!(datas[0].last_name.as_bytes(), marias[0].0);
        assert_eq!(datas[1].last_name.as_bytes(), marias[1].0);

        // The pks
        assert_eq!(pks[0], marias[0].1.key().unwrap());
        assert_eq!(pks[1], marias[1].1.key().unwrap());

        // The associated data
        assert_eq!(datas[0], marias[0].1.value);
        assert_eq!(datas[1], marias[1].1.value);
    }

    #[test]
    fn range_composite_key_by_unique_index() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);

        let res: StdResult<Vec<_>> = DATA
            .idx
            .name_lastname
            .prefix(b"Maria".to_vec())
            .range(&store, None, None, Order::Ascending)
            .collect();
        let marias = res.unwrap();

        // Only two people are called "Maria"
        let count = marias.len();
        assert_eq!(2, count);

        // The ik::suffix
        assert_eq!(datas[0].last_name.as_bytes(), marias[0].0);
        assert_eq!(datas[1].last_name.as_bytes(), marias[1].0);

        // The pks
        assert_eq!(pks[0], marias[0].1.key().unwrap());
        assert_eq!(pks[1], marias[1].1.key().unwrap());

        // The associated data
        assert_eq!(datas[0], marias[0].1.value);
        assert_eq!(datas[1], marias[1].1.value);
    }

    #[test]
    fn range_simple_string_key() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);

        // let's try to iterate!
        let all: StdResult<Vec<_>> = DATA.range(&store, None, None, Order::Ascending).collect();
        let all = all.unwrap();
        assert_eq!(
            all,
            pks.clone()
                .into_iter()
                .map(str::to_string)
                .zip(datas.clone().into_iter())
                .collect::<Vec<_>>()
        );

        // let's try to iterate over a range
        let all: StdResult<Vec<_>> = DATA
            .range(&store, Some(Bound::inclusive("3")), None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(
            all,
            pks.into_iter()
                .map(str::to_string)
                .zip(datas.into_iter())
                .rev()
                .take(3)
                .rev()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn prefix_simple_string_key() {
        let mut store = MockStorage::new();

        // save data
        let (pks, datas) = save_data(&mut store);

        // Let's prefix and iterate.
        // This is similar to calling range() directly, but added here for completeness / prefix
        // type checks

        // Grug note:
        // we changed this. This test doesn't make sense now.
        // it's like to call only range. With prefix, the IK::Prefix in this case is ().
        let all: StdResult<Vec<_>> = DATA
            // .prefix(())
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(
            all,
            pks.clone()
                .into_iter()
                .map(str::to_string)
                .zip(datas.into_iter())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn prefix_composite_key() {
        let mut store = MockStorage::new();

        let indexes = DataCompositeMultiIndex::<(&str, &str)> {
            name_age: MultiIndex::new(
                |_pk, d| index_tuple(&d.name, d.age),
                "data",
                "data__name_age",
            ),
        };
        let map: IndexedMap<(&str, &str), Data, DataCompositeMultiIndex<(&str, &str)>, Borsh> =
            IndexedMap::new("data", indexes);

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk1 = ("1", "5627");
        map.save(&mut store, pk1, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk2 = ("2", "5628");
        map.save(&mut store, pk2, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Young".to_string(),
            age: 24,
        };
        let pk3 = ("2", "5629");
        map.save(&mut store, pk3, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 43,
        };
        let pk4 = ("3", "5630");
        map.save(&mut store, pk4, &data4).unwrap();

        // let's prefix and iterate
        let result: StdResult<Vec<_>> = map
            .prefix("2")
            .range(&store, None, None, Order::Ascending)
            .collect();
        let result = result.unwrap();
        assert_eq!(result, [
            ("5628".to_string(), data2),
            ("5629".to_string(), data3),
        ]);
    }

    #[test]
    fn prefix_triple_key() {
        let mut store = MockStorage::new();

        let indexes = DataCompositeMultiIndex {
            name_age: MultiIndex::new(
                |_pk, d| index_tuple(&d.name, d.age),
                "data",
                "data__name_age",
            ),
        };
        let map: IndexedMap<(&str, &str, &str), Data, DataCompositeMultiIndex<(&str, &str, &str)>> =
            IndexedMap::new("data", indexes);

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk1 = ("1", "1", "5627");
        map.save(&mut store, pk1, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk2 = ("1", "2", "5628");
        map.save(&mut store, pk2, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Young".to_string(),
            age: 24,
        };
        let pk3 = ("2", "1", "5629");
        map.save(&mut store, pk3, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 43,
        };
        let pk4 = ("2", "2", "5630");
        map.save(&mut store, pk4, &data4).unwrap();

        // let's prefix and iterate
        let result: StdResult<Vec<_>> = map
            .prefix("1")
            .append("2")
            .range(&store, None, None, Order::Ascending)
            .collect();
        let result = result.unwrap();
        assert_eq!(result, [("5628".to_string(), data2),]);
    }

    #[test]
    fn sub_prefix_triple_key() {
        let mut store = MockStorage::new();

        let indexes = DataCompositeMultiIndex {
            name_age: MultiIndex::new(
                |_pk, d| index_tuple(&d.name, d.age),
                "data",
                "data__name_age",
            ),
        };
        let map: IndexedMap<(&str, &str, &str), Data, DataCompositeMultiIndex<(&str, &str, &str)>> =
            IndexedMap::new("data", indexes);

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk1 = ("1", "1", "5627");
        map.save(&mut store, pk1, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk2 = ("1", "2", "5628");
        map.save(&mut store, pk2, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Young".to_string(),
            age: 24,
        };
        let pk3 = ("2", "1", "5629");
        map.save(&mut store, pk3, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 43,
        };
        let pk4 = ("2", "2", "5630");
        map.save(&mut store, pk4, &data4).unwrap();

        // let's sub-prefix and iterate
        let result: StdResult<Vec<_>> = map
            .prefix("1")
            .range(&store, None, None, Order::Ascending)
            .collect();
        let result = result.unwrap();
        assert_eq!(result, [
            (("1".to_string(), "5627".to_string()), data1),
            (("2".to_string(), "5628".to_string()), data2),
        ]);
    }

    #[test]
    fn prefix_range_simple_key() {
        let mut store = MockStorage::new();

        let indexes = DataCompositeMultiIndex {
            name_age: MultiIndex::new(
                |_pk, d| index_tuple(&d.name, d.age),
                "data",
                "data__name_age",
            ),
        };
        let map: IndexedMap<(&str, &str), Data, DataCompositeMultiIndex<(&str, &str)>> =
            IndexedMap::new("data", indexes);

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk1 = ("1", "5627");
        map.save(&mut store, pk1, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk2 = ("2", "5628");
        map.save(&mut store, pk2, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Young".to_string(),
            age: 24,
        };
        let pk3 = ("2", "5629");
        map.save(&mut store, pk3, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 43,
        };
        let pk4 = ("3", "5630");
        map.save(&mut store, pk4, &data4).unwrap();

        // let's prefix-range and iterate
        let result: StdResult<Vec<_>> = map
            .prefix_range(
                &store,
                Some(PrefixBound::inclusive("2")),
                None,
                Order::Ascending,
            )
            .collect();
        let result = result.unwrap();
        assert_eq!(result, [
            (("2".to_string(), "5628".to_string()), data2.clone()),
            (("2".to_string(), "5629".to_string()), data3.clone()),
            (("3".to_string(), "5630".to_string()), data4)
        ]);

        // let's try to iterate over a more restrictive prefix-range!
        let result: StdResult<Vec<_>> = map
            .prefix_range(
                &store,
                Some(PrefixBound::inclusive("2")),
                Some(PrefixBound::exclusive("3")),
                Order::Ascending,
            )
            .collect();
        let result = result.unwrap();
        assert_eq!(result, [
            (("2".to_string(), "5628".to_string()), data2),
            (("2".to_string(), "5629".to_string()), data3),
        ]);
    }

    #[test]
    fn prefix_range_triple_key() {
        let mut store = MockStorage::new();

        let indexes = DataCompositeMultiIndex {
            name_age: MultiIndex::new(
                |_pk, d| index_tuple(&d.name, d.age),
                "data",
                "data__name_age",
            ),
        };
        let map: IndexedMap<(&str, &str, &str), Data, DataCompositeMultiIndex<(&str, &str, &str)>> =
            IndexedMap::new("data", indexes);

        // save data
        let data1 = Data {
            name: "Maria".to_string(),
            last_name: "".to_string(),
            age: 42,
        };
        let pk1 = ("1", "1", "5627");
        map.save(&mut store, pk1, &data1).unwrap();

        let data2 = Data {
            name: "Juan".to_string(),
            last_name: "Perez".to_string(),
            age: 13,
        };
        let pk2 = ("1", "2", "5628");
        map.save(&mut store, pk2, &data2).unwrap();

        let data3 = Data {
            name: "Maria".to_string(),
            last_name: "Young".to_string(),
            age: 24,
        };
        let pk3 = ("2", "1", "5629");
        map.save(&mut store, pk3, &data3).unwrap();

        let data4 = Data {
            name: "Maria Luisa".to_string(),
            last_name: "Bemberg".to_string(),
            age: 43,
        };
        let pk4 = ("2", "2", "5630");
        map.save(&mut store, pk4, &data4).unwrap();

        // Grug implementation:
        // on grug the prefix for (A, B, C) is A.
        // Cosmwasm has (A, B) as prefix.

        // let's prefix-range and iterate
        let result: StdResult<Vec<_>> = map
            .prefix_range(
                &store,
                Some(PrefixBound::inclusive("1")),
                None,
                Order::Ascending,
            )
            .collect();
        let result = result.unwrap();
        assert_eq!(result, [
            (
                ("1".to_string(), "1".to_string(), "5627".to_string()),
                data1.clone()
            ),
            (
                ("1".to_string(), "2".to_string(), "5628".to_string()),
                data2.clone()
            ),
            (
                ("2".to_string(), "1".to_string(), "5629".to_string()),
                data3.clone()
            ),
            (
                ("2".to_string(), "2".to_string(), "5630".to_string()),
                data4
            )
        ]);

        // let's prefix-range over inclusive bounds on both sides
        let result: StdResult<Vec<_>> = map
            .prefix_range(
                &store,
                Some(PrefixBound::inclusive("1")),
                Some(PrefixBound::exclusive("2")),
                Order::Ascending,
            )
            .collect();
        let result = result.unwrap();
        assert_eq!(result, [
            (
                ("1".to_string(), "1".to_string(), "5627".to_string()),
                data1.clone()
            ),
            (
                ("1".to_string(), "2".to_string(), "5628".to_string()),
                data2.clone()
            ),
        ]);
    }

    mod bounds_unique_index {
        use super::*;

        struct Indexes<'a, PK> {
            secondary: UniqueIndex<'a, PK, u64, u64>,
        }

        impl<'a, PK> IndexList<'a, PK, u64> for Indexes<'a, PK>
        where
            PK: Key,
        {
            fn get_indexes(&'_ self) -> Box<dyn Iterator<Item = &'_ dyn Index<PK, u64>> + '_> {
                let v: Vec<&dyn Index<PK, u64>> = vec![&self.secondary];
                Box::new(v.into_iter())
            }
        }

        #[test]
        fn composite_key_query() {
            let indexes = Indexes {
                secondary: UniqueIndex::new(|secondary| *secondary, "test_map__secondary"),
            };
            let map = IndexedMap::<&str, u64, Indexes<&str>>::new("test_map", indexes);
            let mut store = MockStorage::new();

            map.save(&mut store, "one", &1).unwrap();
            map.save(&mut store, "two", &2).unwrap();
            map.save(&mut store, "three", &3).unwrap();

            // Inclusive bound
            let items: Vec<_> = map
                .idx
                .secondary
                .values(&store, None, Some(Bound::inclusive(1u64)), Order::Ascending)
                .map(|val| val.unwrap().value)
                .collect();

            // Strip the index from values (for simpler comparison)
            // let items: Vec<_> = items.into_iter().map(|(_, v)| v).collect();

            assert_eq!(items, vec![1]);

            // Exclusive bound
            let items: Vec<_> = map
                .idx
                .secondary
                .values(&store, Some(Bound::exclusive(2u64)), None, Order::Ascending)
                .map(|val| val.unwrap().value)
                .collect();

            assert_eq!(items, vec![3]);
        }
    }

    mod bounds_multi_index {
        use super::*;

        struct Indexes<'a> {
            // The last type param must match the `IndexedMap` primary key type, below
            secondary: MultiIndex<'a, &'a str, u64, u64>,
        }

        impl<'a> IndexList<'a, &'a str, u64> for Indexes<'a> {
            fn get_indexes(&'_ self) -> Box<dyn Iterator<Item = &'_ dyn Index<&'a str, u64>> + '_> {
                let v: Vec<&dyn Index<&str, u64>> = vec![&self.secondary];
                Box::new(v.into_iter())
            }
        }

        #[test]
        fn composite_key_query() {
            let indexes = Indexes {
                secondary: MultiIndex::new(
                    |_pk, secondary| *secondary,
                    "test_map",
                    "test_map__secondary",
                ),
            };
            let map = IndexedMap::<&str, u64, Indexes>::new("test_map", indexes);
            let mut store = MockStorage::new();

            map.save(&mut store, "one", &1).unwrap();
            map.save(&mut store, "two", &2).unwrap();
            map.save(&mut store, "two2", &2).unwrap();
            map.save(&mut store, "three", &3).unwrap();

            // TODO: Grug note: we don't have prefix_range_raw implemented
            // leaving this here for future implementation

            // Inclusive prefix-bound
            // let items: Vec<_> = map
            //     .idx
            //     .secondary
            //     .prefix_range_raw()
            //     .range(&store, None, Some(Bound::inclusive(1u64)), Order::Ascending)
            //     .collect::<Result<_, _>>()
            //     .unwrap();

            // Strip the index from values (for simpler comparison)
            // let items: Vec<_> = items.into_iter().map(|(_, v)| v).collect();

            // assert_eq!(items, vec![1]);

            // Exclusive bound (used for pagination)
            // Range over the index specifying a primary key (multi-index key includes the pk)
            let items: Vec<_> = map
                .idx
                .secondary
                .range(
                    &store,
                    Some(Bound::exclusive((2u64, "two"))),
                    None,
                    Order::Ascending,
                )
                .collect::<Result<_, _>>()
                .unwrap();

            assert_eq!(items, vec![
                (2, "two2".to_string(), 2),
                (3, "three".to_string(), 3)
            ]);
        }
    }

    mod pk_multi_index {
        use {
            super::*,
            grug_types::{Addr, Uint128},
        };

        struct Indexes<'a> {
            // The last type param must match the `IndexedMap` primary key type below
            spender: MultiIndex<'a, (&'a Addr, &'a Addr), Addr, Uint128>,
        }

        impl<'a> IndexList<'a, (&'a Addr, &'a Addr), Uint128> for Indexes<'a> {
            fn get_indexes(
                &'_ self,
            ) -> Box<dyn Iterator<Item = &'_ dyn Index<(&'a Addr, &'a Addr), Uint128>> + '_>
            {
                let v: Vec<&dyn Index<(&Addr, &Addr), Uint128>> = vec![&self.spender];
                Box::new(v.into_iter())
            }
        }

        #[test]
        fn pk_based_index() {
            let indexes = Indexes {
                spender: MultiIndex::new(
                    |pk, _allow| pk.1.clone(),
                    "allowances",
                    "allowances__spender",
                ),
            };
            let map: IndexedMap<(&Addr, &Addr), grug_types::Uint<u128>, Indexes> =
                IndexedMap::new("allowances", indexes);
            let mut store = MockStorage::new();

            let owner_1 = Addr::mock(1);
            let owner_2 = Addr::mock(2);
            let spender_1 = Addr::mock(3);
            let spender_2 = Addr::mock(4);

            map.save(&mut store, (&owner_1, &spender_1), &Uint128::new(11))
                .unwrap();
            map.save(&mut store, (&owner_1, &spender_2), &Uint128::new(12))
                .unwrap();
            map.save(&mut store, (&owner_2, &spender_1), &Uint128::new(21))
                .unwrap();

            // Iterate over the main values
            let items: Vec<_> = map
                .range_raw(&store, None, None, Order::Ascending)
                .collect();

            // Strip the index from values (for simpler comparison)
            let items: Vec<u128> = items
                .into_iter()
                .map(|(_, v)| from_borsh_slice::<_, Uint128>(v).unwrap().into())
                .collect();

            assert_eq!(items, vec![11, 12, 21]);

            // Iterate over the indexed values
            let items = map
                .idx
                .spender
                .range(&store, None, None, Order::Ascending)
                .map(|val| val.unwrap().2)
                .collect::<Vec<_>>();

            assert_eq!(items, vec![11_u128.into(), 21_u128.into(), 12_u128.into()]);

            // Prefix over the main values
            let items: Vec<_> = map
                .prefix(&owner_1)
                .range(&store, None, None, Order::Ascending)
                .collect::<StdResult<_>>()
                .unwrap();

            // Strip the index from values (for simpler comparison)
            let items: Vec<u128> = items.into_iter().map(|(_, v)| v.into()).collect();

            assert_eq!(items, vec![11, 12]);

            // Prefix over the indexed values
            let items: Vec<_> = map
                .idx
                .spender
                .prefix(spender_1.clone())
                .range(&store, None, None, Order::Ascending)
                .collect::<Result<_, _>>()
                .unwrap();

            // Strip the index from values (for simpler comparison)
            let items: Vec<u128> = items.into_iter().map(|(_, v)| v.into()).collect();

            assert_eq!(items, vec![11, 21]);

            // Prefix over the indexed values, and deserialize primary key as well
            let items: Vec<_> = map
                .idx
                .spender
                .prefix(spender_2.clone())
                .range(&store, None, None, Order::Ascending)
                .collect::<Result<_, _>>()
                .unwrap();

            assert_eq!(items, vec![((owner_1, spender_2), Uint128::new(12))]);
        }
    }

    #[test]
    fn clear_works() {
        let mut storage = MockStorage::new();
        let (pks, _) = save_data(&mut storage);

        DATA.clear(&mut storage, None, None);

        for key in pks {
            assert!(!DATA.has(&storage, key));
        }
    }

    #[test]
    fn is_empty_works() {
        let mut storage = MockStorage::new();

        assert!(DATA.is_empty(&storage));

        save_data(&mut storage);

        assert!(!DATA.is_empty(&storage));
    }
}
