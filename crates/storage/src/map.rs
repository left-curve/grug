use {
    crate::{Borsh, Bound, Codec, Key, PathBuf, Prefix, PrefixBound, Prefixer},
    grug_types::{Order, Record, StdError, StdResult, Storage},
    std::{borrow::Cow, marker::PhantomData},
};

pub struct Map<'a, K, T, C = Borsh>
where
    C: Codec<T>,
{
    namespace: &'a [u8],
    key: PhantomData<K>,
    data: PhantomData<T>,
    codec: PhantomData<C>,
}

impl<'a, K, T, C> Map<'a, K, T, C>
where
    C: Codec<T>,
{
    pub const fn new(namespace: &'a str) -> Self {
        // TODO: add a maximum length for namespace
        // see comments of increment_last_byte function for rationale
        Self {
            namespace: namespace.as_bytes(),
            key: PhantomData,
            data: PhantomData,
            codec: PhantomData,
        }
    }
}

impl<'a, K, T, C> Map<'a, K, T, C>
where
    K: Key,
    C: Codec<T>,
{
    fn path_raw(&self, key_raw: &[u8]) -> PathBuf<T, C> {
        PathBuf::new(self.namespace, &[], Some(&Cow::Borrowed(key_raw)))
    }

    #[doc(hidden)]
    pub fn path(&self, key: K) -> PathBuf<T, C> {
        let mut raw_keys = key.raw_keys();
        let last_raw_key = raw_keys.pop();
        PathBuf::new(self.namespace, &raw_keys, last_raw_key.as_ref())
    }

    fn no_prefix(&self) -> Prefix<K, T, C> {
        Prefix::new(self.namespace, &[])
    }

    pub fn prefix(&self, prefix: K::Prefix) -> Prefix<K::Suffix, T, C> {
        Prefix::new(self.namespace, &prefix.raw_prefixes())
    }

    pub fn is_empty(&self, storage: &dyn Storage) -> bool {
        self.no_prefix().is_empty(storage)
    }

    // ---------------------- methods for single entries -----------------------

    pub fn has_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> bool {
        self.path_raw(key_raw).as_path().exists(storage)
    }

    pub fn has(&self, storage: &dyn Storage, key: K) -> bool {
        self.path(key).as_path().exists(storage)
    }

    pub fn may_load_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> Option<Vec<u8>> {
        self.path_raw(key_raw).as_path().may_load_raw(storage)
    }

    pub fn may_load(&self, storage: &dyn Storage, key: K) -> StdResult<Option<T>> {
        self.path(key).as_path().may_load(storage)
    }

    pub fn load_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> StdResult<Vec<u8>> {
        self.path_raw(key_raw).as_path().load_raw(storage)
    }

    pub fn load(&self, storage: &dyn Storage, key: K) -> StdResult<T> {
        self.path(key).as_path().load(storage)
    }

    /// Using this function is not recommended. If the key or data isn't
    /// properly serialized, later when you read the data, it will fail to
    /// deserialize and error.
    ///
    /// We prefix the function name with the word "unsafe" to highlight this.
    pub fn unsafe_save_raw(&self, storage: &mut dyn Storage, key_raw: &[u8], data_raw: &[u8]) {
        self.path_raw(key_raw).as_path().save_raw(storage, data_raw)
    }

    pub fn save(&self, storage: &mut dyn Storage, key: K, data: &T) -> StdResult<()> {
        self.path(key).as_path().save(storage, data)
    }

    pub fn remove_raw(&self, storage: &mut dyn Storage, key_raw: &[u8]) {
        self.path_raw(key_raw).as_path().remove(storage)
    }

    pub fn remove(&self, storage: &mut dyn Storage, key: K) {
        self.path(key).as_path().remove(storage)
    }

    pub fn update<A, Err>(
        &self,
        storage: &mut dyn Storage,
        key: K,
        action: A,
    ) -> Result<Option<T>, Err>
    where
        A: FnOnce(Option<T>) -> Result<Option<T>, Err>,
        Err: From<StdError>,
    {
        self.path(key).as_path().update(storage, action)
    }

    // -------------------- iteration methods (full bound) ---------------------

    pub fn range_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'b> {
        self.no_prefix().range_raw(storage, min, max, order)
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
        storage: &'b dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.no_prefix().keys_raw(storage, min, max, order)
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

    // ------------------- iteration methods (prefix bound) --------------------

    pub fn prefix_range_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'b> {
        self.no_prefix().prefix_range_raw(storage, min, max, order)
    }

    pub fn prefix_range<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(K::Output, T)>> + 'b> {
        self.no_prefix().prefix_range(storage, min, max, order)
    }

    pub fn prefix_keys_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.no_prefix().prefix_keys_raw(storage, min, max, order)
    }

    pub fn prefix_keys<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<K::Output>> + 'b> {
        self.no_prefix().prefix_keys(storage, min, max, order)
    }

    pub fn prefix_values_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.no_prefix().prefix_values_raw(storage, min, max, order)
    }

    pub fn prefix_values<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'b> {
        self.no_prefix().prefix_values(storage, min, max, order)
    }

    pub fn prefix_clear(
        &self,
        storage: &mut dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
    ) {
        self.no_prefix().prefix_clear(storage, min, max)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod test {
    use {
        crate::{Map, PrefixBound},
        borsh::{BorshDeserialize, BorshSerialize},
        grug_types::{MockStorage, StdResult},
    };

    const FOOS: Map<u64, Foo> = Map::new("foo");

    #[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq)]
    struct Foo {
        name: String,
        surname: String,
    }

    impl Foo {
        pub fn new(name: &str, surname: &str) -> Self {
            Self {
                name: name.to_string(),
                surname: surname.to_string(),
            }
        }
    }

    fn setup_test() -> MockStorage {
        let mut storage = MockStorage::new();

        for (key, name, surname) in [
            (1, "name_1", "surname_1"),
            (2, "name_2", "surname_2"),
            (3, "name_3", "surname_3"),
            (4, "name_4", "surname_4"),
        ] {
            FOOS.save(&mut storage, key, &Foo::new(name, surname))
                .unwrap();
        }

        storage
    }

    #[test]
    fn map_works() {
        let storage = setup_test();

        let first = FOOS.load(&storage, 1).unwrap();
        assert_eq!(first, Foo::new("name_1", "surname_1"));
    }

    #[test]
    fn range_prefix() {
        const MAP: Map<(u64, &str), String> = Map::new("foo");

        let mut storage = MockStorage::new();

        for (index, addr, desc) in [
            (1, "name_1", "desc_1"),
            (2, "name_2", "desc_2"),
            (2, "name_3", "desc_3"),
            (3, "name_4", "desc_4"),
            (3, "name_5", "desc_5"),
            (4, "name_6", "desc_6"),
        ] {
            MAP.save(&mut storage, (index, addr), &desc.to_string())
                .unwrap();
        }

        // `prefix_range` with a max bound, ascending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    None,
                    Some(PrefixBound::inclusive(2_u64)),
                    grug_types::Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(res, vec![
                ((1_u64, "name_1".to_string()), "desc_1".to_string()),
                ((2_u64, "name_2".to_string()), "desc_2".to_string()),
                ((2_u64, "name_3".to_string()), "desc_3".to_string()),
            ]);
        }

        // `prefix_range` with a min bound, ascending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    Some(PrefixBound::exclusive(2_u64)),
                    None,
                    grug_types::Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(res, vec![
                ((3_u64, "name_4".to_string()), "desc_4".to_string()),
                ((3_u64, "name_5".to_string()), "desc_5".to_string()),
                ((4_u64, "name_6".to_string()), "desc_6".to_string()),
            ]);
        }

        // `prefix_range` with a max bound, Descending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    None,
                    Some(PrefixBound::exclusive(2_u64)),
                    grug_types::Order::Descending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            #[rustfmt::skip]
            assert_eq!(res, vec![
                ((1_u64, "name_1".to_string()), "desc_1".to_string()),
            ]);
        }

        // `prefix_range` with a min bound, Descending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    Some(PrefixBound::inclusive(2_u64)),
                    None,
                    grug_types::Order::Descending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(res, vec![
                ((4_u64, "name_6".to_string()), "desc_6".to_string()),
                ((3_u64, "name_5".to_string()), "desc_5".to_string()),
                ((3_u64, "name_4".to_string()), "desc_4".to_string()),
                ((2_u64, "name_3".to_string()), "desc_3".to_string()),
                ((2_u64, "name_2".to_string()), "desc_2".to_string()),
            ]);
        }

        // `prefix_range` with both min and max bounds, ascending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    Some(PrefixBound::inclusive(2_u64)),
                    Some(PrefixBound::exclusive(4_u64)),
                    grug_types::Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(res, vec![
                ((2_u64, "name_2".to_string()), "desc_2".to_string()),
                ((2_u64, "name_3".to_string()), "desc_3".to_string()),
                ((3_u64, "name_4".to_string()), "desc_4".to_string()),
                ((3_u64, "name_5".to_string()), "desc_5".to_string()),
            ]);
        }
    }
}

#[cfg(test)]
mod cosmwasm_test {
    use {
        super::Map,
        crate::{Bound, Key, PrefixBound},
        borsh::{BorshDeserialize, BorshSerialize},
        grug_types::{from_borsh_slice, to_borsh_vec, MockStorage, Order, StdResult, Storage},
    };

    #[derive(BorshDeserialize, BorshSerialize, PartialEq, Debug, Clone)]
    struct Data {
        pub name: String,
        pub age: i32,
    }

    const PEOPLE: Map<&[u8], Data> = Map::new("people");
    const PEOPLE_STR_KEY: &str = "people2";
    const PEOPLE_STR: Map<&str, Data> = Map::new(PEOPLE_STR_KEY);
    const PEOPLE_ID: Map<u32, Data> = Map::new("people_id");
    const SIGNED_ID: Map<i32, Data> = Map::new("signed_id");

    const ALLOWANCE: Map<(&[u8], &[u8]), u64> = Map::new("allow");

    const TRIPLE: Map<(&[u8], u8, &str), u64> = Map::new("triple");

    #[test]
    fn create_path() {
        let path = PEOPLE.path(b"john");
        let key = path.storage_key;
        // this should be prefixed(people) || john
        assert_eq!("people".len() + "john".len() + 2, key.len());
        assert_eq!(b"people".to_vec().as_slice(), &key[2..8]);
        assert_eq!(b"john".to_vec().as_slice(), &key[8..]);

        let path = ALLOWANCE.path((b"john", b"maria"));
        let key = path.storage_key;
        // this should be prefixed(allow) || prefixed(john) || maria
        assert_eq!(
            "allow".len() + "john".len() + "maria".len() + 2 * 2,
            key.len()
        );
        assert_eq!(b"allow".to_vec().as_slice(), &key[2..7]);
        assert_eq!(b"john".to_vec().as_slice(), &key[9..13]);
        assert_eq!(b"maria".to_vec().as_slice(), &key[13..]);

        let path = TRIPLE.path((b"john", 8u8, "pedro"));
        let key = path.storage_key;
        // this should be prefixed(triple) || prefixed(john) || prefixed(8u8) || pedro
        assert_eq!(
            "triple".len() + "john".len() + 1 + "pedro".len() + 2 * 3,
            key.len()
        );
        assert_eq!(b"triple".to_vec().as_slice(), &key[2..8]);
        assert_eq!(b"john".to_vec().as_slice(), &key[10..14]);
        assert_eq!(8u8.to_be_bytes(), &key[16..17]);
        assert_eq!(b"pedro".to_vec().as_slice(), &key[17..]);
    }

    #[test]
    fn save_and_load() {
        let mut store = MockStorage::new();

        // save and load on one key
        let john = PEOPLE.path(b"john");
        let john = john.as_path();
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        assert_eq!(None, john.may_load(&store).unwrap());
        john.save(&mut store, &data).unwrap();
        assert_eq!(data, john.load(&store).unwrap());

        // nothing on another key
        assert_eq!(None, PEOPLE.may_load(&store, b"jack").unwrap());

        // same named path gets the data
        assert_eq!(data, PEOPLE.load(&store, b"john").unwrap());

        // removing leaves us empty
        john.remove(&mut store);
        assert_eq!(None, john.may_load(&store).unwrap());
    }

    #[test]
    fn existence() {
        let mut store = MockStorage::new();

        // set data in proper format
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, b"john", &data).unwrap();

        // set and remove it
        PEOPLE.save(&mut store, b"removed", &data).unwrap();
        PEOPLE.remove(&mut store, b"removed");

        // invalid, but non-empty data
        store.write(&PEOPLE.path(b"random").storage_key, b"random-data");

        // any data, including invalid or empty is returned as "has"
        assert!(PEOPLE.has(&store, b"john"));
        assert!(PEOPLE.has(&store, b"random"));

        // if nothing was written, it is false
        assert!(!PEOPLE.has(&store, b"never-writen"));
        assert!(!PEOPLE.has(&store, b"removed"));
    }

    #[test]
    fn composite_keys() {
        let mut store = MockStorage::new();

        // save and load on a composite key
        let allow = ALLOWANCE.path((b"owner", b"spender"));
        let allow = allow.as_path();
        assert_eq!(None, allow.may_load(&store).unwrap());
        allow.save(&mut store, &1234).unwrap();
        assert_eq!(1234, allow.load(&store).unwrap());

        // not under other key
        let different = ALLOWANCE.may_load(&store, (b"owners", b"pender")).unwrap();
        assert_eq!(None, different);

        // matches under a proper copy
        let same = ALLOWANCE.load(&store, (b"owner", b"spender")).unwrap();
        assert_eq!(1234, same);
    }

    #[test]
    fn triple_keys() {
        let mut store = MockStorage::new();

        // save and load on a triple composite key
        let triple = TRIPLE.path((b"owner", 10u8, "recipient"));
        let triple = triple.as_path();
        assert_eq!(None, triple.may_load(&store).unwrap());
        triple.save(&mut store, &1234).unwrap();
        assert_eq!(1234, triple.load(&store).unwrap());

        // not under other key
        let different = TRIPLE
            .may_load(&store, (b"owners", 10u8, "ecipient"))
            .unwrap();
        assert_eq!(None, different);

        // matches under a proper copy
        let same = TRIPLE.load(&store, (b"owner", 10u8, "recipient")).unwrap();
        assert_eq!(1234, same);
    }

    #[test]
    fn range_raw_simple_key() {
        let mut store = MockStorage::new();

        // save and load on two keys
        let data1 = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, b"john", &data1).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut store, b"jim", &data2).unwrap();

        let data_1_raw = to_borsh_vec(&data1).unwrap();
        let data_2_raw = to_borsh_vec(&data2).unwrap();

        // let's try to iterate!
        let all: Vec<_> = PEOPLE
            .range_raw(&store, None, None, Order::Ascending)
            .collect();

        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            (b"jim".to_vec(), data_2_raw.clone()),
            (b"john".to_vec(), data_1_raw.clone())
        ]);

        // let's try to iterate over a range
        let all: Vec<_> = PEOPLE
            .range_raw(
                &store,
                Some(Bound::inclusive(b"j" as &[u8])),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            (b"jim".to_vec(), data_2_raw),
            (b"john".to_vec(), data_1_raw.clone())
        ]);

        // let's try to iterate over a more restrictive range
        let all: Vec<_> = PEOPLE
            .range_raw(
                &store,
                Some(Bound::inclusive(b"jo" as &[u8])),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(b"john".to_vec(), data_1_raw)]);
    }

    #[test]
    fn range_simple_string_key() {
        let mut store = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, b"john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut store, b"jim", &data2).unwrap();

        let data3 = Data {
            name: "Ada".to_string(),
            age: 23,
        };
        PEOPLE.save(&mut store, b"ada", &data3).unwrap();

        // let's try to iterate!
        let all: StdResult<Vec<_>> = PEOPLE.range(&store, None, None, Order::Ascending).collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(all, vec![
            (b"ada".to_vec(), data3),
            (b"jim".to_vec(), data2.clone()),
            (b"john".to_vec(), data.clone())
        ]);

        // let's try to iterate over a range
        let all: StdResult<Vec<_>> = PEOPLE
            .range(
                &store,
                Some(Bound::inclusive(b"j".as_slice())),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            (b"jim".to_vec(), data2),
            (b"john".to_vec(), data.clone())
        ]);

        // let's try to iterate over a more restrictive range
        let all: StdResult<Vec<_>> = PEOPLE
            .range(
                &store,
                Some(Bound::inclusive(b"jo".as_slice())),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(b"john".to_vec(), data)]);
    }

    #[test]
    fn range_key_broken_deserialization_errors() {
        let mut store = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_STR.save(&mut store, "john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_STR.save(&mut store, "jim", &data2).unwrap();

        let data3 = Data {
            name: "Ada".to_string(),
            age: 23,
        };
        PEOPLE_STR.save(&mut store, "ada", &data3).unwrap();

        // let's iterate!
        let all: StdResult<Vec<_>> = PEOPLE_STR
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(all, vec![
            ("ada".to_string(), data3.clone()),
            ("jim".to_string(), data2.clone()),
            ("john".to_string(), data.clone())
        ]);

        // Manually add a broken key (invalid utf-8)
        store.write(
            &[
                [0u8, PEOPLE_STR_KEY.len() as u8].as_slice(),
                PEOPLE_STR_KEY.as_bytes(),
                b"\xddim",
            ]
            .concat(),
            &to_borsh_vec(&data2).unwrap(),
        );

        // Let's try to iterate again!
        let all: StdResult<Vec<_>> = PEOPLE_STR
            .range(&store, None, None, Order::Ascending)
            .collect();
        assert!(all.is_err());

        // And the same with keys()
        let all: StdResult<Vec<_>> = PEOPLE_STR
            .keys(&store, None, None, Order::Ascending)
            .collect();
        assert!(all.is_err());

        // But range_raw still works
        let all: Vec<_> = PEOPLE_STR
            .range_raw(&store, None, None, Order::Ascending)
            .collect();

        assert_eq!(4, all.len());
        assert_eq!(all, vec![
            (b"ada".to_vec(), to_borsh_vec(&data3).unwrap()),
            (b"jim".to_vec(), to_borsh_vec(&data2).unwrap()),
            (b"john".to_vec(), to_borsh_vec(&data).unwrap()),
            (b"\xddim".to_vec(), to_borsh_vec(&data2).unwrap()),
        ]);

        // And the same with keys_raw
        let all: Vec<_> = PEOPLE_STR
            .keys_raw(&store, None, None, Order::Ascending)
            .collect();

        assert_eq!(4, all.len());
        assert_eq!(all, vec![
            b"ada".to_vec(),
            b"jim".to_vec(),
            b"john".to_vec(),
            b"\xddim".to_vec(),
        ]);
    }

    #[test]
    fn range_simple_integer_key() {
        let mut store = MockStorage::new();

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_ID.save(&mut store, 1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_ID.save(&mut store, 56, &data2).unwrap();

        // let's try to iterate!
        let all: StdResult<Vec<_>> = PEOPLE_ID
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2.clone()), (1234, data.clone())]);

        // let's try to iterate over a range
        let all: StdResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                Some(Bound::inclusive(56u32)),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2), (1234, data.clone())]);

        // let's try to iterate over a more restrictive range
        let all: StdResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                Some(Bound::inclusive(57u32)),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(1234, data)]);
    }

    #[test]
    fn range_simple_integer_key_with_bounder_trait() {
        let mut store = MockStorage::new();

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_ID.save(&mut store, 1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_ID.save(&mut store, 56, &data2).unwrap();

        // let's try to iterate!
        let all: StdResult<Vec<_>> = PEOPLE_ID
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2.clone()), (1234, data.clone())]);

        // let's try to iterate over a range
        let all: StdResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                Some(Bound::inclusive(56u32)),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2), (1234, data.clone())]);

        // let's try to iterate over a more restrictive range
        let all: StdResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                Some(Bound::inclusive(57u32)),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(1234, data)]);
    }

    #[test]
    fn range_simple_signed_integer_key() {
        let mut store = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        SIGNED_ID.save(&mut store, -1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        SIGNED_ID.save(&mut store, -56, &data2).unwrap();

        let data3 = Data {
            name: "Jules".to_string(),
            age: 55,
        };
        SIGNED_ID.save(&mut store, 50, &data3).unwrap();

        // let's try to iterate!
        let all: StdResult<Vec<_>> = SIGNED_ID
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        // order is correct
        assert_eq!(all, vec![
            (-1234, data),
            (-56, data2.clone()),
            (50, data3.clone())
        ]);

        // let's try to iterate over a range
        let all: StdResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                Some(Bound::inclusive(-56i32)),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(-56, data2), (50, data3.clone())]);

        // let's try to iterate over a more restrictive range
        let all: StdResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                Some(Bound::inclusive(-55i32)),
                Some(Bound::inclusive(50i32)),
                Order::Descending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(50, data3)]);
    }

    #[test]
    fn range_simple_signed_integer_key_with_bounder_trait() {
        let mut store = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        SIGNED_ID.save(&mut store, -1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        SIGNED_ID.save(&mut store, -56, &data2).unwrap();

        let data3 = Data {
            name: "Jules".to_string(),
            age: 55,
        };
        SIGNED_ID.save(&mut store, 50, &data3).unwrap();

        // let's try to iterate!
        let all: StdResult<Vec<_>> = SIGNED_ID
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        // order is correct
        assert_eq!(all, vec![
            (-1234, data),
            (-56, data2.clone()),
            (50, data3.clone())
        ]);

        // let's try to iterate over a range
        let all: StdResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                Some(Bound::inclusive(-56i32)),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(-56, data2), (50, data3.clone())]);

        // let's try to iterate over a more restrictive range
        let all: StdResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                Some(Bound::inclusive(-55i32)),
                Some(Bound::inclusive(50i32)),
                Order::Descending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(50, data3)]);
    }

    #[test]
    fn range_raw_composite_key() {
        let mut store = MockStorage::new();

        // save and load on three keys, one under different owner
        ALLOWANCE
            .save(&mut store, (b"owner", b"spender"), &1000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, (b"owner", b"spender2"), &3000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, (b"owner2", b"spender"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: Vec<_> = ALLOWANCE
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(3, all.len());
        assert_eq!(all, vec![
            (
                (b"owner".to_vec(), b"spender".to_vec()).joined_key(),
                to_borsh_vec(&1000_u64).unwrap()
            ),
            (
                (b"owner".to_vec(), b"spender2".to_vec()).joined_key(),
                to_borsh_vec(&3000_u64).unwrap()
            ),
            (
                (b"owner2".to_vec(), b"spender".to_vec()).joined_key(),
                to_borsh_vec(&5000_u64).unwrap()
            ),
        ]);

        // let's try to iterate over a prefix
        let all: Vec<_> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            (b"spender".to_vec(), to_borsh_vec(&1000_u64).unwrap()),
            (b"spender2".to_vec(), to_borsh_vec(&3000_u64).unwrap())
        ]);
    }

    #[test]
    fn range_composite_key() {
        let mut store = MockStorage::new();

        // save and load on three keys, one under different owner
        ALLOWANCE
            .save(&mut store, (b"owner", b"spender"), &1000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, (b"owner", b"spender2"), &3000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, (b"owner2", b"spender"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: StdResult<Vec<_>> = ALLOWANCE
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(all, vec![
            ((b"owner".to_vec(), b"spender".to_vec()), 1000),
            ((b"owner".to_vec(), b"spender2".to_vec()), 3000),
            ((b"owner2".to_vec(), b"spender".to_vec()), 5000)
        ]);

        // let's try to iterate over a prefix
        let all: StdResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            (b"spender".to_vec(), 1000),
            (b"spender2".to_vec(), 3000),
        ]);

        // let's try to iterate over a prefixed restricted inclusive range
        let all: StdResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range(
                &store,
                Some(Bound::inclusive(b"spender".as_slice())),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            (b"spender".to_vec(), 1000),
            (b"spender2".to_vec(), 3000),
        ]);

        // let's try to iterate over a prefixed restricted exclusive range
        let all: StdResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range(
                &store,
                Some(Bound::exclusive(b"spender".as_slice())),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(b"spender2".to_vec(), 3000),]);
    }

    #[test]
    fn range_raw_triple_key() {
        let mut store = MockStorage::new();

        // save and load on three keys, one under different owner
        TRIPLE
            .save(&mut store, (b"owner", 9, "recipient"), &1000)
            .unwrap();
        TRIPLE
            .save(&mut store, (b"owner", 9, "recipient2"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, (b"owner", 10, "recipient3"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, (b"owner2", 9, "recipient"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: Vec<_> = TRIPLE
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(4, all.len());
        assert_eq!(all, vec![
            (
                (b"owner".to_vec(), 9u8, b"recipient".to_vec()).joined_key(),
                to_borsh_vec(&1000_u64).unwrap()
            ),
            (
                (b"owner".to_vec(), 9u8, b"recipient2".to_vec()).joined_key(),
                to_borsh_vec(&3000_u64).unwrap()
            ),
            (
                (b"owner".to_vec(), 10u8, b"recipient3".to_vec()).joined_key(),
                to_borsh_vec(&3000_u64).unwrap()
            ),
            (
                (b"owner2".to_vec(), 9u8, b"recipient".to_vec()).joined_key(),
                to_borsh_vec(&5000_u64).unwrap()
            )
        ]);

        // let's iterate over a prefix
        let all: Vec<_> = TRIPLE
            .prefix(b"owner")
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(3, all.len());
        assert_eq!(all, vec![
            (
                (9u8, b"recipient".to_vec()).joined_key(),
                to_borsh_vec(&1000_u64).unwrap()
            ),
            (
                (9u8, b"recipient2".to_vec()).joined_key(),
                to_borsh_vec(&3000_u64).unwrap()
            ),
            (
                (10u8, b"recipient3".to_vec()).joined_key(),
                to_borsh_vec(&3000_u64).unwrap()
            )
        ]);

        // let's iterate over a sub prefix
        let all: Vec<_> = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(2, all.len());
        // Use range() if you want key deserialization
        assert_eq!(all, vec![
            (
                (b"recipient".to_vec()).joined_key(),
                to_borsh_vec(&1000_u64).unwrap()
            ),
            (
                (b"recipient2".to_vec()).joined_key(),
                to_borsh_vec(&3000_u64).unwrap()
            ),
        ]);
    }

    #[test]
    fn range_triple_key() {
        let mut store = MockStorage::new();

        // save and load on three keys, one under different owner
        TRIPLE
            .save(&mut store, (b"owner", 9u8, "recipient"), &1000)
            .unwrap();
        TRIPLE
            .save(&mut store, (b"owner", 9u8, "recipient2"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, (b"owner", 10u8, "recipient3"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, (b"owner2", 9u8, "recipient"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: StdResult<Vec<_>> = TRIPLE.range(&store, None, None, Order::Ascending).collect();
        let all = all.unwrap();
        assert_eq!(4, all.len());
        assert_eq!(all, vec![
            ((b"owner".to_vec(), 9, "recipient".to_string()), 1000),
            ((b"owner".to_vec(), 9, "recipient2".to_string()), 3000),
            ((b"owner".to_vec(), 10, "recipient3".to_string()), 3000),
            ((b"owner2".to_vec(), 9, "recipient".to_string()), 5000)
        ]);

        // let's iterate over a sub_prefix
        let all: StdResult<Vec<_>> = TRIPLE
            .prefix(b"owner")
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(all, vec![
            ((9, "recipient".to_string()), 1000),
            ((9, "recipient2".to_string()), 3000),
            ((10, "recipient3".to_string()), 3000),
        ]);

        // let's iterate over a prefix
        let all: StdResult<Vec<_>> = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range(&store, None, None, Order::Ascending)
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            ("recipient".to_string(), 1000),
            ("recipient2".to_string(), 3000),
        ]);

        // let's try to iterate over a prefixed restricted inclusive range
        let all: StdResult<Vec<_>> = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range(
                &store,
                Some(Bound::inclusive("recipient")),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![
            ("recipient".to_string(), 1000),
            ("recipient2".to_string(), 3000),
        ]);

        // let's try to iterate over a prefixed restricted exclusive range
        let all: StdResult<Vec<_>> = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range(
                &store,
                Some(Bound::exclusive("recipient")),
                None,
                Order::Ascending,
            )
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![("recipient2".to_string(), 3000),]);
    }

    #[test]
    fn basic_update() {
        let mut store = MockStorage::new();

        let add_ten = |a: Option<u64>| -> StdResult<_> { Ok(Some(a.unwrap_or_default() + 10)) };

        // save and load on three keys, one under different owner
        let key: (&[u8], &[u8]) = (b"owner", b"spender");
        ALLOWANCE.update(&mut store, key, add_ten).unwrap();
        let twenty = ALLOWANCE.update(&mut store, key, add_ten).unwrap().unwrap();
        assert_eq!(20, twenty);
        let loaded = ALLOWANCE.load(&store, key).unwrap();
        assert_eq!(20, loaded);
    }

    #[test]
    fn readme_works() -> StdResult<()> {
        let mut store = MockStorage::new();
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };

        // load and save with extra key argument
        let empty = PEOPLE.may_load(&store, b"john")?;
        assert_eq!(None, empty);
        PEOPLE.save(&mut store, b"john", &data)?;
        let loaded = PEOPLE.load(&store, b"john")?;
        assert_eq!(data, loaded);

        // nothing on another key
        let missing = PEOPLE.may_load(&store, b"jack")?;
        assert_eq!(None, missing);

        // update function for new or existing keys
        let birthday = |d: Option<Data>| -> StdResult<Option<Data>> {
            match d {
                Some(one) => Ok(Some(Data {
                    name: one.name,
                    age: one.age + 1,
                })),
                None => Ok(Some(Data {
                    name: "Newborn".to_string(),
                    age: 0,
                })),
            }
        };

        let old_john = PEOPLE.update(&mut store, b"john", birthday)?.unwrap();
        assert_eq!(33, old_john.age);
        assert_eq!("John", old_john.name.as_str());

        let new_jack = PEOPLE.update(&mut store, b"jack", birthday)?.unwrap();
        assert_eq!(0, new_jack.age);
        assert_eq!("Newborn", new_jack.name.as_str());

        // update also changes the store
        assert_eq!(old_john, PEOPLE.load(&store, b"john")?);
        assert_eq!(new_jack, PEOPLE.load(&store, b"jack")?);

        // removing leaves us empty
        PEOPLE.remove(&mut store, b"john");
        let empty = PEOPLE.may_load(&store, b"john")?;
        assert_eq!(None, empty);

        Ok(())
    }

    #[test]
    fn readme_works_composite_keys() -> StdResult<()> {
        let mut store = MockStorage::new();

        // save and load on a composite key
        let empty = ALLOWANCE.may_load(&store, (b"owner", b"spender"))?;
        assert_eq!(None, empty);
        ALLOWANCE.save(&mut store, (b"owner", b"spender"), &777)?;
        let loaded = ALLOWANCE.load(&store, (b"owner", b"spender"))?;
        assert_eq!(777, loaded);

        // doesn't appear under other key (even if a concat would be the same)
        let different = ALLOWANCE.may_load(&store, (b"owners", b"pender")).unwrap();
        assert_eq!(None, different);

        // simple update
        ALLOWANCE.update(
            &mut store,
            (b"owner", b"spender"),
            |v| -> StdResult<Option<u64>> { Ok(Some(v.unwrap_or_default() + 222)) },
        )?;
        let loaded = ALLOWANCE.load(&store, (b"owner", b"spender"))?;
        assert_eq!(999, loaded);

        Ok(())
    }

    #[test]
    fn readme_works_with_path() -> StdResult<()> {
        let mut store = MockStorage::new();
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };

        // create a Path one time to use below
        let john = PEOPLE.path(b"john");
        let john = john.as_path();

        // Use this just like an Item above
        let empty = john.may_load(&store)?;
        assert_eq!(None, empty);
        john.save(&mut store, &data)?;
        let loaded = john.load(&store)?;
        assert_eq!(data, loaded);
        john.remove(&mut store);
        let empty = john.may_load(&store)?;
        assert_eq!(None, empty);

        // same for composite keys, just use both parts in key()
        let allow = ALLOWANCE.path((b"owner", b"spender"));
        let allow = allow.as_path();
        allow.save(&mut store, &1234)?;
        let loaded = allow.load(&store)?;
        assert_eq!(1234, loaded);
        allow.update(&mut store, |x| -> StdResult<Option<u64>> {
            Ok(Some(x.unwrap_or_default() * 2))
        })?;
        let loaded = allow.load(&store)?;
        assert_eq!(2468, loaded);

        Ok(())
    }

    #[test]
    fn readme_with_range_raw() -> StdResult<()> {
        let mut store = MockStorage::new();

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, b"john", &data)?;
        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut store, b"jim", &data2)?;

        // iterate over them all
        let all: Vec<_> = PEOPLE
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(all, vec![
            (b"jim".to_vec(), to_borsh_vec(&data2)?),
            (b"john".to_vec(), to_borsh_vec(&data)?)
        ]);

        // or just show what is after jim
        let all: Vec<_> = PEOPLE
            .range_raw(
                &store,
                Some(Bound::exclusive(b"jim" as &[u8])),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(all, vec![(b"john".to_vec(), to_borsh_vec(&data)?)]);

        // save and load on three keys, one under different owner
        ALLOWANCE.save(&mut store, (b"owner", b"spender"), &1000)?;
        ALLOWANCE.save(&mut store, (b"owner", b"spender2"), &3000)?;
        ALLOWANCE.save(&mut store, (b"owner2", b"spender"), &5000)?;

        // get all under one key
        let all: Vec<_> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(all, vec![
            (b"spender".to_vec(), to_borsh_vec(&1000_u64)?),
            (b"spender2".to_vec(), to_borsh_vec(&3000_u64)?)
        ]);

        // Or ranges between two items (even reverse)
        let all: Vec<_> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(
                &store,
                Some(Bound::exclusive(b"spender1" as &[u8])),
                Some(Bound::inclusive(b"spender2" as &[u8])),
                Order::Descending,
            )
            .collect();
        assert_eq!(all, vec![(b"spender2".to_vec(), to_borsh_vec(&3000_u64)?)]);

        Ok(())
    }

    #[test]
    fn prefixed_range_raw_works() {
        // this is designed to look as much like a secondary index as possible
        // we want to query over a range of u32 for the first key and all subkeys
        const AGES: Map<(u32, Vec<u8>), u64> = Map::new("ages");

        let mut store = MockStorage::new();
        AGES.save(&mut store, (2, vec![1, 2, 3]), &123).unwrap();
        AGES.save(&mut store, (3, vec![4, 5, 6]), &456).unwrap();
        AGES.save(&mut store, (5, vec![7, 8, 9]), &789).unwrap();
        AGES.save(&mut store, (5, vec![9, 8, 7]), &987).unwrap();
        AGES.save(&mut store, (7, vec![20, 21, 22]), &2002).unwrap();
        AGES.save(&mut store, (8, vec![23, 24, 25]), &2332).unwrap();

        // typical range under one prefix as a control
        let fives = AGES
            .prefix(5)
            .range_raw(&store, None, None, Order::Ascending)
            .collect::<Vec<_>>();

        assert_eq!(fives.len(), 2);
        assert_eq!(fives, vec![
            (vec![7, 8, 9], to_borsh_vec(&789_u64).unwrap()),
            (vec![9, 8, 7], to_borsh_vec(&987_u64).unwrap())
        ]);

        let keys: Vec<_> = AGES
            .keys_raw(&store, None, None, Order::Ascending)
            .collect();
        println!("keys: {:?}", keys);

        // using inclusive bounds both sides
        let include = AGES
            .prefix_range_raw(
                &store,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| from_borsh_slice(r.1).unwrap())
            .collect::<Vec<u64>>();
        assert_eq!(include.len(), 4);
        assert_eq!(include, vec![456, 789, 987, 2002]);

        // using exclusive bounds both sides
        let exclude = AGES
            .prefix_range_raw(
                &store,
                Some(PrefixBound::exclusive(3u32)),
                Some(PrefixBound::exclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| from_borsh_slice(r.1).unwrap())
            .collect::<Vec<u64>>();

        assert_eq!(exclude.len(), 2);
        assert_eq!(exclude, vec![789, 987]);

        // using inclusive in descending
        let include = AGES
            .prefix_range_raw(
                &store,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(5u32)),
                Order::Descending,
            )
            .map(|r| from_borsh_slice(r.1).unwrap())
            .collect::<Vec<u64>>();
        assert_eq!(include.len(), 3);
        assert_eq!(include, vec![987, 789, 456]);

        // using exclusive in descending
        let include = AGES
            .prefix_range_raw(
                &store,
                Some(PrefixBound::exclusive(2u32)),
                Some(PrefixBound::exclusive(5u32)),
                Order::Descending,
            )
            .map(|r| from_borsh_slice(r.1).unwrap())
            .collect::<Vec<u64>>();
        assert_eq!(include.len(), 1);
        assert_eq!(include, vec![456]);
    }

    #[test]
    fn prefixed_range_works() {
        // this is designed to look as much like a secondary index as possible
        // we want to query over a range of u32 for the first key and all subkeys
        const AGES: Map<(u32, &str), u64> = Map::new("ages");

        let mut store = MockStorage::new();
        AGES.save(&mut store, (2, "123"), &123).unwrap();
        AGES.save(&mut store, (3, "456"), &456).unwrap();
        AGES.save(&mut store, (5, "789"), &789).unwrap();
        AGES.save(&mut store, (5, "987"), &987).unwrap();
        AGES.save(&mut store, (7, "202122"), &2002).unwrap();
        AGES.save(&mut store, (8, "232425"), &2332).unwrap();

        // typical range under one prefix as a control
        let fives = AGES
            .prefix(5)
            .range(&store, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(fives.len(), 2);
        assert_eq!(fives, vec![
            ("789".to_string(), 789),
            ("987".to_string(), 987)
        ]);

        let keys: Vec<_> = AGES.keys(&store, None, None, Order::Ascending).collect();
        println!("keys: {:?}", keys);

        // using inclusive bounds both sides
        let include = AGES
            .prefix_range(
                &store,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 4);
        assert_eq!(include, vec![456, 789, 987, 2002]);

        // using exclusive bounds both sides
        let exclude = AGES
            .prefix_range(
                &store,
                Some(PrefixBound::exclusive(3u32)),
                Some(PrefixBound::exclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(exclude.len(), 2);
        assert_eq!(exclude, vec![789, 987]);

        // using inclusive in descending
        let include = AGES
            .prefix_range(
                &store,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(5u32)),
                Order::Descending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 3);
        assert_eq!(include, vec![987, 789, 456]);

        // using exclusive in descending
        let include = AGES
            .prefix_range(
                &store,
                Some(PrefixBound::exclusive(2u32)),
                Some(PrefixBound::exclusive(5u32)),
                Order::Descending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 1);
        assert_eq!(include, vec![456]);
    }

    #[test]
    fn clear_works() {
        const TEST_MAP: Map<&str, u32> = Map::new("test_map");

        let mut storage = MockStorage::new();
        TEST_MAP.save(&mut storage, "key0", &0u32).unwrap();
        TEST_MAP.save(&mut storage, "key1", &1u32).unwrap();
        TEST_MAP.save(&mut storage, "key2", &2u32).unwrap();
        TEST_MAP.save(&mut storage, "key3", &3u32).unwrap();
        TEST_MAP.save(&mut storage, "key4", &4u32).unwrap();

        TEST_MAP.clear(&mut storage, None, None);

        assert!(!TEST_MAP.has(&storage, "key0"));
        assert!(!TEST_MAP.has(&storage, "key1"));
        assert!(!TEST_MAP.has(&storage, "key2"));
        assert!(!TEST_MAP.has(&storage, "key3"));
        assert!(!TEST_MAP.has(&storage, "key4"));

        let mut storage = MockStorage::new();
        TEST_MAP.save(&mut storage, "key0", &0u32).unwrap();
        TEST_MAP.save(&mut storage, "key1", &1u32).unwrap();
        TEST_MAP.save(&mut storage, "key2", &2u32).unwrap();
        TEST_MAP.save(&mut storage, "key3", &3u32).unwrap();
        TEST_MAP.save(&mut storage, "key4", &4u32).unwrap();

        TEST_MAP.clear(
            &mut storage,
            Some(Bound::inclusive("key0")),
            Some(Bound::exclusive("key3")),
        );

        assert!(!TEST_MAP.has(&storage, "key0"));
        assert!(!TEST_MAP.has(&storage, "key1"));
        assert!(!TEST_MAP.has(&storage, "key2"));
        assert!(TEST_MAP.has(&storage, "key3"));
        assert!(TEST_MAP.has(&storage, "key4"));
    }

    #[test]
    fn is_empty_works() {
        const TEST_MAP: Map<&str, u32> = Map::new("test_map");

        let mut storage = MockStorage::new();

        assert!(TEST_MAP.is_empty(&storage));

        TEST_MAP.save(&mut storage, "key1", &1u32).unwrap();
        TEST_MAP.save(&mut storage, "key2", &2u32).unwrap();

        assert!(!TEST_MAP.is_empty(&storage));
    }
}
