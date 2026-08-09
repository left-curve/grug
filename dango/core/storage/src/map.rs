use {
    crate::{Borsh, Codec, Path, Prefix, PrefixBound, Prefixer, PrimaryKey, RawKey},
    dango_primitives::{Bound, Order, Record, StdError, StdResult, Storage},
    std::{collections::BTreeMap, marker::PhantomData},
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

impl<K, T, C> Map<'_, K, T, C>
where
    K: PrimaryKey,
    C: Codec<T>,
{
    pub fn path_raw(&self, key_raw: &[u8]) -> Path<'_, T, C> {
        Path::new(self.namespace, &[], Some(RawKey::Borrowed(key_raw)))
    }

    pub fn path(&self, key: K) -> Path<'_, T, C> {
        let mut raw_keys = key.raw_keys();
        let last_raw_key = raw_keys.pop();
        Path::new(self.namespace, &raw_keys, last_raw_key)
    }

    pub fn no_prefix(&self) -> Prefix<K, T, C> {
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
        self.path_raw(key_raw).exists(storage)
    }

    pub fn has(&self, storage: &dyn Storage, key: K) -> bool {
        self.path(key).exists(storage)
    }

    pub fn may_load_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> Option<Vec<u8>> {
        self.path_raw(key_raw).may_load_raw(storage)
    }

    pub fn may_load(&self, storage: &dyn Storage, key: K) -> StdResult<Option<T>> {
        self.path(key).may_load(storage)
    }

    pub fn load_raw(&self, storage: &dyn Storage, key_raw: &[u8]) -> StdResult<Vec<u8>> {
        self.path_raw(key_raw).load_raw(storage)
    }

    pub fn load(&self, storage: &dyn Storage, key: K) -> StdResult<T> {
        self.path(key).load(storage)
    }

    pub fn may_take_raw(&self, storage: &mut dyn Storage, key_raw: &[u8]) -> Option<Vec<u8>> {
        self.path_raw(key_raw).may_take_raw(storage)
    }

    pub fn may_take(&self, storage: &mut dyn Storage, key: K) -> StdResult<Option<T>> {
        self.path(key).may_take(storage)
    }

    pub fn take_raw(&self, storage: &mut dyn Storage, key_raw: &[u8]) -> StdResult<Vec<u8>> {
        self.path_raw(key_raw).take_raw(storage)
    }

    pub fn take(&self, storage: &mut dyn Storage, key: K) -> StdResult<T> {
        self.path(key).take(storage)
    }

    /// Using this function is not recommended. If the key or data isn't
    /// properly serialized, later when you read the data, it will fail to
    /// deserialize and error.
    ///
    /// We prefix the function name with the word "unsafe" to highlight this.
    pub fn unsafe_save_raw(&self, storage: &mut dyn Storage, key_raw: &[u8], data_raw: &[u8]) {
        self.path_raw(key_raw).save_raw(storage, data_raw)
    }

    pub fn save(&self, storage: &mut dyn Storage, key: K, data: &T) -> StdResult<()> {
        self.path(key).save(storage, data)
    }

    pub fn remove_raw(&self, storage: &mut dyn Storage, key_raw: &[u8]) {
        self.path_raw(key_raw).remove(storage)
    }

    pub fn remove(&self, storage: &mut dyn Storage, key: K) {
        self.path(key).remove(storage)
    }

    pub fn may_update<F, E>(&self, storage: &mut dyn Storage, key: K, action: F) -> Result<T, E>
    where
        F: FnOnce(Option<T>) -> Result<T, E>,
        E: From<StdError>,
    {
        self.path(key).may_update(storage, action)
    }

    pub fn update<F, E>(&self, storage: &mut dyn Storage, key: K, action: F) -> Result<T, E>
    where
        F: FnOnce(T) -> Result<T, E>,
        E: From<StdError>,
    {
        self.path(key).update(storage, action)
    }

    pub fn may_modify<F, E>(
        &self,
        storage: &mut dyn Storage,
        key: K,
        action: F,
    ) -> Result<Option<T>, E>
    where
        F: FnOnce(Option<T>) -> Result<Option<T>, E>,
        E: From<StdError>,
    {
        self.path(key).may_modify(storage, action)
    }

    pub fn modify<F, E>(&self, storage: &mut dyn Storage, key: K, action: F) -> Result<Option<T>, E>
    where
        F: FnOnce(T) -> Result<Option<T>, E>,
        E: From<StdError>,
    {
        self.path(key).modify(storage, action)
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

    pub fn drain(
        &self,
        storage: &mut dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
    ) -> StdResult<BTreeMap<K::Output, T>>
    where
        K: Clone,
        K::Output: Ord,
    {
        self.no_prefix().drain(storage, min, max)
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
        dango_primitives::{MockStorage, StdResult},
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
                    Some(PrefixBound::Inclusive(2_u64)),
                    dango_primitives::Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                res,
                vec![
                    ((1_u64, "name_1".to_string()), "desc_1".to_string()),
                    ((2_u64, "name_2".to_string()), "desc_2".to_string()),
                    ((2_u64, "name_3".to_string()), "desc_3".to_string()),
                ]
            );
        }

        // `prefix_range` with a min bound, ascending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    Some(PrefixBound::Exclusive(2_u64)),
                    None,
                    dango_primitives::Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                res,
                vec![
                    ((3_u64, "name_4".to_string()), "desc_4".to_string()),
                    ((3_u64, "name_5".to_string()), "desc_5".to_string()),
                    ((4_u64, "name_6".to_string()), "desc_6".to_string()),
                ]
            );
        }

        // `prefix_range` with a max bound, Descending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    None,
                    Some(PrefixBound::Exclusive(2_u64)),
                    dango_primitives::Order::Descending,
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
                    Some(PrefixBound::Inclusive(2_u64)),
                    None,
                    dango_primitives::Order::Descending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                res,
                vec![
                    ((4_u64, "name_6".to_string()), "desc_6".to_string()),
                    ((3_u64, "name_5".to_string()), "desc_5".to_string()),
                    ((3_u64, "name_4".to_string()), "desc_4".to_string()),
                    ((2_u64, "name_3".to_string()), "desc_3".to_string()),
                    ((2_u64, "name_2".to_string()), "desc_2".to_string()),
                ]
            );
        }

        // `prefix_range` with both min and max bounds, ascending
        {
            let res = MAP
                .prefix_range(
                    &storage,
                    Some(PrefixBound::Inclusive(2_u64)),
                    Some(PrefixBound::Exclusive(4_u64)),
                    dango_primitives::Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                res,
                vec![
                    ((2_u64, "name_2".to_string()), "desc_2".to_string()),
                    ((2_u64, "name_3".to_string()), "desc_3".to_string()),
                    ((3_u64, "name_4".to_string()), "desc_4".to_string()),
                    ((3_u64, "name_5".to_string()), "desc_5".to_string()),
                ]
            );
        }
    }
}

// ---------------------- tests copied over from cosmwasm ----------------------

#[cfg(test)]
mod cosmwasm_tests {
    use {
        crate::{Map, PrefixBound, PrimaryKey},
        borsh::{BorshDeserialize, BorshSerialize},
        dango_primitives::{
            BorshDeExt, BorshSerExt, Bound, MockStorage, Order, StdResult, Storage,
        },
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
        let key = path.storage_key();

        // this should be prefixed(people) || john
        assert_eq!("people".len() + "john".len() + 2, key.len());
        assert_eq!(b"people".to_vec().as_slice(), &key[2..8]);
        assert_eq!(b"john".to_vec().as_slice(), &key[8..]);

        let path = ALLOWANCE.path((b"john", b"maria"));
        let key = path.storage_key();

        // this should be prefixed(allow) || prefixed(john) || maria
        assert_eq!(
            "allow".len() + "john".len() + "maria".len() + 2 * 2,
            key.len()
        );
        assert_eq!(b"allow".to_vec().as_slice(), &key[2..7]);
        assert_eq!(b"john".to_vec().as_slice(), &key[9..13]);
        assert_eq!(b"maria".to_vec().as_slice(), &key[13..]);

        let path = TRIPLE.path((b"john", 8u8, "pedro"));
        let key = path.storage_key();

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
        let mut storage = MockStorage::new();

        // save and load on one key
        let john = PEOPLE.path(b"john");
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        assert_eq!(None, john.may_load(&storage).unwrap());

        john.save(&mut storage, &data).unwrap();
        assert_eq!(data, john.load(&storage).unwrap());

        // nothing on another key
        assert_eq!(None, PEOPLE.may_load(&storage, b"jack").unwrap());

        // same named path gets the data
        assert_eq!(data, PEOPLE.load(&storage, b"john").unwrap());

        // removing leaves us empty
        john.remove(&mut storage);
        assert_eq!(None, john.may_load(&storage).unwrap());
    }

    #[test]
    fn existence() {
        let mut storage = MockStorage::new();

        // set data in proper format
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut storage, b"john", &data).unwrap();

        // set and remove it
        PEOPLE.save(&mut storage, b"removed", &data).unwrap();
        PEOPLE.remove(&mut storage, b"removed");

        // invalid, but non-empty data
        storage.write(PEOPLE.path(b"random").storage_key(), b"random-data");

        // any data, including invalid or empty is returned as "has"
        assert!(PEOPLE.has(&storage, b"john"));
        assert!(PEOPLE.has(&storage, b"random"));

        // if nothing was written, it is false
        assert!(!PEOPLE.has(&storage, b"never-writen"));
        assert!(!PEOPLE.has(&storage, b"removed"));
    }

    #[test]
    fn composite_keys() {
        let mut storage = MockStorage::new();

        // save and load on a composite key
        let allow = ALLOWANCE.path((b"owner", b"spender"));
        assert_eq!(None, allow.may_load(&storage).unwrap());

        allow.save(&mut storage, &1234).unwrap();
        assert_eq!(1234, allow.load(&storage).unwrap());

        // not under other key
        let different = ALLOWANCE
            .may_load(&storage, (b"owners", b"pender"))
            .unwrap();
        assert_eq!(None, different);

        // matches under a proper copy
        let same = ALLOWANCE.load(&storage, (b"owner", b"spender")).unwrap();
        assert_eq!(1234, same);
    }

    #[test]
    fn triple_keys() {
        let mut storage = MockStorage::new();

        // save and load on a triple composite key
        let triple = TRIPLE.path((b"owner", 10u8, "recipient"));
        assert_eq!(None, triple.may_load(&storage).unwrap());

        triple.save(&mut storage, &1234).unwrap();
        assert_eq!(1234, triple.load(&storage).unwrap());

        // not under other key
        let different = TRIPLE
            .may_load(&storage, (b"owners", 10u8, "ecipient"))
            .unwrap();
        assert_eq!(None, different);

        // matches under a proper copy
        let same = TRIPLE
            .load(&storage, (b"owner", 10u8, "recipient"))
            .unwrap();
        assert_eq!(1234, same);
    }

    #[test]
    fn range_raw_simple_key() {
        let mut storage = MockStorage::new();

        // save and load on two keys
        let data1 = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut storage, b"john", &data1).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut storage, b"jim", &data2).unwrap();

        let data_1_raw = data1.to_borsh_vec().unwrap();
        let data_2_raw = data2.to_borsh_vec().unwrap();

        // let's try to iterate!
        let all: Vec<_> = PEOPLE
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();

        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![
                (b"jim".to_vec(), data_2_raw.clone()),
                (b"john".to_vec(), data_1_raw.clone())
            ]
        );

        // let's try to iterate over a range
        let all: Vec<_> = PEOPLE
            .range_raw(
                &storage,
                Some(Bound::Inclusive(b"j")),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![
                (b"jim".to_vec(), data_2_raw),
                (b"john".to_vec(), data_1_raw.clone())
            ]
        );

        // let's try to iterate over a more restrictive range
        let all: Vec<_> = PEOPLE
            .range_raw(
                &storage,
                Some(Bound::Inclusive(b"jo")),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(b"john".to_vec(), data_1_raw)]);
    }

    #[test]
    fn range_simple_string_key() {
        let mut storage = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut storage, b"john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut storage, b"jim", &data2).unwrap();

        let data3 = Data {
            name: "Ada".to_string(),
            age: 23,
        };
        PEOPLE.save(&mut storage, b"ada", &data3).unwrap();

        // let's try to iterate!
        let all = PEOPLE
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [
                (b"ada".to_vec(), data3),
                (b"jim".to_vec(), data2.clone()),
                (b"john".to_vec(), data.clone())
            ]
        );

        // let's try to iterate over a range
        let all = PEOPLE
            .range(
                &storage,
                Some(Bound::Inclusive(b"j".as_slice())),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [(b"jim".to_vec(), data2), (b"john".to_vec(), data.clone())]
        );

        // let's try to iterate over a more restrictive range
        let all = PEOPLE
            .range(
                &storage,
                Some(Bound::Inclusive(b"jo".as_slice())),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(b"john".to_vec(), data)]);
    }

    #[test]
    fn range_key_broken_deserialization_errors() {
        let mut storage = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_STR.save(&mut storage, "john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_STR.save(&mut storage, "jim", &data2).unwrap();

        let data3 = Data {
            name: "Ada".to_string(),
            age: 23,
        };
        PEOPLE_STR.save(&mut storage, "ada", &data3).unwrap();

        // let's iterate!
        let all = PEOPLE_STR
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [
                ("ada".to_string(), data3.clone()),
                ("jim".to_string(), data2.clone()),
                ("john".to_string(), data.clone())
            ]
        );

        // Manually add a broken key (invalid utf-8)
        storage.write(
            &[
                [0u8, PEOPLE_STR_KEY.len() as u8].as_slice(),
                PEOPLE_STR_KEY.as_bytes(),
                b"\xddim",
            ]
            .concat(),
            &data2.to_borsh_vec().unwrap(),
        );

        // Let's try to iterate again!
        let all: StdResult<Vec<_>> = PEOPLE_STR
            .range(&storage, None, None, Order::Ascending)
            .collect();
        assert!(all.is_err());

        // And the same with keys()
        let all: StdResult<Vec<_>> = PEOPLE_STR
            .keys(&storage, None, None, Order::Ascending)
            .collect();
        assert!(all.is_err());

        // But range_raw still works
        let all: Vec<_> = PEOPLE_STR
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                (b"ada".to_vec(), data3.to_borsh_vec().unwrap()),
                (b"jim".to_vec(), data2.to_borsh_vec().unwrap()),
                (b"john".to_vec(), data.to_borsh_vec().unwrap()),
                (b"\xddim".to_vec(), data2.to_borsh_vec().unwrap()),
            ]
        );

        // And the same with keys_raw
        let all: Vec<_> = PEOPLE_STR
            .keys_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                b"ada".to_vec(),
                b"jim".to_vec(),
                b"john".to_vec(),
                b"\xddim".to_vec(),
            ]
        );
    }

    #[test]
    fn range_simple_integer_key() {
        let mut storage = MockStorage::new();

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_ID.save(&mut storage, 1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_ID.save(&mut storage, 56, &data2).unwrap();

        // let's try to iterate!
        let all = PEOPLE_ID
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();

        assert_eq!(all, [(56, data2.clone()), (1234, data.clone())]);

        // let's try to iterate over a range
        let all = PEOPLE_ID
            .range(
                &storage,
                Some(Bound::Inclusive(56u32)),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(56, data2), (1234, data.clone())]);

        // let's try to iterate over a more restrictive range
        let all = PEOPLE_ID
            .range(
                &storage,
                Some(Bound::Inclusive(57u32)),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(1234, data)]);
    }

    #[test]
    fn range_simple_integer_key_with_bounder_trait() {
        let mut storage = MockStorage::new();

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_ID.save(&mut storage, 1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_ID.save(&mut storage, 56, &data2).unwrap();

        // let's try to iterate!
        let all = PEOPLE_ID
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(56, data2.clone()), (1234, data.clone())]);

        // let's try to iterate over a range
        let all = PEOPLE_ID
            .range(
                &storage,
                Some(Bound::Inclusive(56u32)),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(56, data2), (1234, data.clone())]);

        // let's try to iterate over a more restrictive range
        let all = PEOPLE_ID
            .range(
                &storage,
                Some(Bound::Inclusive(57u32)),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(1234, data)]);
    }

    #[test]
    fn range_simple_signed_integer_key() {
        let mut storage = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        SIGNED_ID.save(&mut storage, -1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        SIGNED_ID.save(&mut storage, -56, &data2).unwrap();

        let data3 = Data {
            name: "Jules".to_string(),
            age: 55,
        };
        SIGNED_ID.save(&mut storage, 50, &data3).unwrap();

        // let's try to iterate!
        let all = SIGNED_ID
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        // order is correct
        assert_eq!(
            all,
            [(-1234, data), (-56, data2.clone()), (50, data3.clone())]
        );

        // let's try to iterate over a range
        let all = SIGNED_ID
            .range(
                &storage,
                Some(Bound::Inclusive(-56i32)),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(-56, data2), (50, data3.clone())]);

        // let's try to iterate over a more restrictive range
        let all = SIGNED_ID
            .range(
                &storage,
                Some(Bound::Inclusive(-55i32)),
                Some(Bound::Inclusive(50i32)),
                Order::Descending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(50, data3)]);
    }

    #[test]
    fn range_simple_signed_integer_key_with_bounder_trait() {
        let mut storage = MockStorage::new();

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        SIGNED_ID.save(&mut storage, -1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        SIGNED_ID.save(&mut storage, -56, &data2).unwrap();

        let data3 = Data {
            name: "Jules".to_string(),
            age: 55,
        };
        SIGNED_ID.save(&mut storage, 50, &data3).unwrap();

        // let's try to iterate!
        let all = SIGNED_ID
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        // order is correct
        assert_eq!(
            all,
            [(-1234, data), (-56, data2.clone()), (50, data3.clone())]
        );

        // let's try to iterate over a range
        let all = SIGNED_ID
            .range(
                &storage,
                Some(Bound::Inclusive(-56i32)),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(-56, data2), (50, data3.clone())]);

        // let's try to iterate over a more restrictive range
        let all = SIGNED_ID
            .range(
                &storage,
                Some(Bound::Inclusive(-55i32)),
                Some(Bound::Inclusive(50i32)),
                Order::Descending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(50, data3)]);
    }

    #[test]
    fn range_raw_composite_key() {
        let mut storage = MockStorage::new();

        // save and load on three keys, one under different owner
        ALLOWANCE
            .save(&mut storage, (b"owner", b"spender"), &1000)
            .unwrap();
        ALLOWANCE
            .save(&mut storage, (b"owner", b"spender2"), &3000)
            .unwrap();
        ALLOWANCE
            .save(&mut storage, (b"owner2", b"spender"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: Vec<_> = ALLOWANCE
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                (
                    (b"owner".to_vec(), b"spender".to_vec()).joined_key(),
                    1000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (b"owner".to_vec(), b"spender2".to_vec()).joined_key(),
                    3000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (b"owner2".to_vec(), b"spender".to_vec()).joined_key(),
                    5000_u64.to_borsh_vec().unwrap()
                ),
            ]
        );

        // let's try to iterate over a prefix
        let all: Vec<_> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                (b"spender".to_vec(), 1000_u64.to_borsh_vec().unwrap()),
                (b"spender2".to_vec(), 3000_u64.to_borsh_vec().unwrap())
            ]
        );
    }

    #[test]
    fn range_composite_key() {
        let mut storage = MockStorage::new();

        // save and load on three keys, one under different owner
        ALLOWANCE
            .save(&mut storage, (b"owner", b"spender"), &1000)
            .unwrap();
        ALLOWANCE
            .save(&mut storage, (b"owner", b"spender2"), &3000)
            .unwrap();
        ALLOWANCE
            .save(&mut storage, (b"owner2", b"spender"), &5000)
            .unwrap();

        // let's try to iterate!
        let all = ALLOWANCE
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [
                ((b"owner".to_vec(), b"spender".to_vec()), 1000),
                ((b"owner".to_vec(), b"spender2".to_vec()), 3000),
                ((b"owner2".to_vec(), b"spender".to_vec()), 5000)
            ]
        );

        // let's try to iterate over a prefix
        let all = ALLOWANCE
            .prefix(b"owner")
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [(b"spender".to_vec(), 1000), (b"spender2".to_vec(), 3000),]
        );

        // let's try to iterate over a prefixed restricted inclusive range
        let all = ALLOWANCE
            .prefix(b"owner")
            .range(
                &storage,
                Some(Bound::Inclusive(b"spender".as_slice())),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [(b"spender".to_vec(), 1000), (b"spender2".to_vec(), 3000),]
        );

        // let's try to iterate over a prefixed restricted exclusive range
        let all = ALLOWANCE
            .prefix(b"owner")
            .range(
                &storage,
                Some(Bound::Exclusive(b"spender".as_slice())),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [(b"spender2".to_vec(), 3000)]);
    }

    #[test]
    fn range_raw_triple_key() {
        let mut storage = MockStorage::new();

        // save and load on three keys, one under different owner
        TRIPLE
            .save(&mut storage, (b"owner", 9, "recipient"), &1000)
            .unwrap();
        TRIPLE
            .save(&mut storage, (b"owner", 9, "recipient2"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut storage, (b"owner", 10, "recipient3"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut storage, (b"owner2", 9, "recipient"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: Vec<_> = TRIPLE
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                (
                    (b"owner".to_vec(), 9u8, b"recipient".to_vec()).joined_key(),
                    1000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (b"owner".to_vec(), 9u8, b"recipient2".to_vec()).joined_key(),
                    3000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (b"owner".to_vec(), 10u8, b"recipient3".to_vec()).joined_key(),
                    3000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (b"owner2".to_vec(), 9u8, b"recipient".to_vec()).joined_key(),
                    5000_u64.to_borsh_vec().unwrap()
                )
            ]
        );

        // let's iterate over a prefix
        let all: Vec<_> = TRIPLE
            .prefix(b"owner")
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                (
                    (9u8, b"recipient".to_vec()).joined_key(),
                    1000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (9u8, b"recipient2".to_vec()).joined_key(),
                    3000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (10u8, b"recipient3".to_vec()).joined_key(),
                    3000_u64.to_borsh_vec().unwrap()
                )
            ]
        );

        // let's iterate over a sub prefix
        let all: Vec<_> = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        // Use range() if you want key deserialization
        assert_eq!(
            all,
            [
                (
                    (b"recipient".to_vec()).joined_key(),
                    1000_u64.to_borsh_vec().unwrap()
                ),
                (
                    (b"recipient2".to_vec()).joined_key(),
                    3000_u64.to_borsh_vec().unwrap()
                ),
            ]
        );
    }

    #[test]
    fn range_triple_key() {
        let mut storage = MockStorage::new();

        // save and load on three keys, one under different owner
        TRIPLE
            .save(&mut storage, (b"owner", 9u8, "recipient"), &1000)
            .unwrap();
        TRIPLE
            .save(&mut storage, (b"owner", 9u8, "recipient2"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut storage, (b"owner", 10u8, "recipient3"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut storage, (b"owner2", 9u8, "recipient"), &5000)
            .unwrap();

        // let's try to iterate!
        let all = TRIPLE
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [
                ((b"owner".to_vec(), 9, "recipient".to_string()), 1000),
                ((b"owner".to_vec(), 9, "recipient2".to_string()), 3000),
                ((b"owner".to_vec(), 10, "recipient3".to_string()), 3000),
                ((b"owner2".to_vec(), 9, "recipient".to_string()), 5000)
            ]
        );

        // let's iterate over a sub_prefix
        let all = TRIPLE
            .prefix(b"owner")
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [
                ((9, "recipient".to_string()), 1000),
                ((9, "recipient2".to_string()), 3000),
                ((10, "recipient3".to_string()), 3000),
            ]
        );

        // let's iterate over a prefix
        let all = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [
                ("recipient".to_string(), 1000),
                ("recipient2".to_string(), 3000),
            ]
        );

        // let's try to iterate over a prefixed restricted inclusive range
        let all = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range(
                &storage,
                Some(Bound::Inclusive("recipient")),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            all,
            [
                ("recipient".to_string(), 1000),
                ("recipient2".to_string(), 3000),
            ]
        );

        // let's try to iterate over a prefixed restricted exclusive range
        let all = TRIPLE
            .prefix(b"owner")
            .append(9)
            .range(
                &storage,
                Some(Bound::Exclusive("recipient")),
                None,
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(all, [("recipient2".to_string(), 3000),]);
    }

    #[test]
    fn basic_update() {
        let mut storage = MockStorage::new();

        let add_ten = |a: Option<u64>| -> StdResult<_> { Ok(Some(a.unwrap_or_default() + 10)) };

        // save and load on three keys, one under different owner
        let key: (&[u8], &[u8]) = (b"owner", b"spender");
        ALLOWANCE.may_modify(&mut storage, key, add_ten).unwrap();

        let twenty = ALLOWANCE
            .may_modify(&mut storage, key, add_ten)
            .unwrap()
            .unwrap();
        assert_eq!(20, twenty);

        let loaded = ALLOWANCE.load(&storage, key).unwrap();
        assert_eq!(20, loaded);
    }

    #[test]
    fn readme_works() {
        let mut storage = MockStorage::new();
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };

        // load and save with extra key argument
        assert!(PEOPLE.may_load(&storage, b"john").unwrap().is_none());

        PEOPLE.save(&mut storage, b"john", &data).unwrap();
        assert_eq!(PEOPLE.load(&storage, b"john").unwrap(), data);

        // nothing on another key
        assert!(PEOPLE.may_load(&storage, b"jack").unwrap().is_none());

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

        let old_john = PEOPLE
            .may_modify(&mut storage, b"john", birthday)
            .unwrap()
            .unwrap();
        assert_eq!(old_john.age, 33);
        assert_eq!(old_john.name, "John");

        let new_jack = PEOPLE
            .may_modify(&mut storage, b"jack", birthday)
            .unwrap()
            .unwrap();
        assert_eq!(new_jack.age, 0);
        assert_eq!(new_jack.name, "Newborn");

        // update also changes the storage
        assert_eq!(old_john, PEOPLE.load(&storage, b"john").unwrap());
        assert_eq!(new_jack, PEOPLE.load(&storage, b"jack").unwrap());

        // removing leaves us empty
        PEOPLE.remove(&mut storage, b"john");
        assert!(PEOPLE.may_load(&storage, b"john").unwrap().is_none());
    }

    #[test]
    fn readme_works_composite_keys() {
        let mut storage = MockStorage::new();

        // save and load on a composite key
        let empty = ALLOWANCE
            .may_load(&storage, (b"owner", b"spender"))
            .unwrap();
        assert_eq!(None, empty);

        ALLOWANCE
            .save(&mut storage, (b"owner", b"spender"), &777)
            .unwrap();

        let loaded = ALLOWANCE.load(&storage, (b"owner", b"spender")).unwrap();
        assert_eq!(777, loaded);

        // doesn't appear under other key (even if a concat would be the same)
        let different = ALLOWANCE
            .may_load(&storage, (b"owners", b"pender"))
            .unwrap();
        assert_eq!(None, different);

        // simple update
        ALLOWANCE
            .may_modify(
                &mut storage,
                (b"owner", b"spender"),
                |v| -> StdResult<Option<u64>> { Ok(Some(v.unwrap_or_default() + 222)) },
            )
            .unwrap();

        let loaded = ALLOWANCE.load(&storage, (b"owner", b"spender")).unwrap();
        assert_eq!(999, loaded);
    }

    #[test]
    fn readme_works_with_path() {
        let mut storage = MockStorage::new();
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };

        // create a Path one time to use below
        {
            let john = PEOPLE.path(b"john");

            // Use this just like an Item above
            assert!(john.may_load(&storage).unwrap().is_none());

            john.save(&mut storage, &data).unwrap();
            assert_eq!(john.load(&storage).unwrap(), data);

            john.remove(&mut storage);
            assert!(john.may_load(&storage).unwrap().is_none());
        }

        // same for composite keys, just use both parts in key()
        {
            let allow = ALLOWANCE.path((b"owner", b"spender"));

            allow.save(&mut storage, &1234).unwrap();
            assert_eq!(allow.load(&storage).unwrap(), 1234);

            allow
                .may_modify(&mut storage, |x| -> StdResult<Option<u64>> {
                    Ok(Some(x.unwrap_or_default() * 2))
                })
                .unwrap();
            assert_eq!(allow.load(&storage).unwrap(), 2468);
        }
    }

    #[test]
    fn readme_with_range_raw() {
        let mut storage = MockStorage::new();

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut storage, b"john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut storage, b"jim", &data2).unwrap();

        // iterate over them all
        let all: Vec<_> = PEOPLE
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                (b"jim".to_vec(), data2.to_borsh_vec().unwrap()),
                (b"john".to_vec(), data.to_borsh_vec().unwrap())
            ]
        );

        // or just show what is after jim
        let all: Vec<_> = PEOPLE
            .range_raw(
                &storage,
                Some(Bound::Exclusive(b"jim")),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(all, [(b"john".to_vec(), data.to_borsh_vec().unwrap())]);

        // save and load on three keys, one under different owner
        ALLOWANCE
            .save(&mut storage, (b"owner", b"spender"), &1000)
            .unwrap();
        ALLOWANCE
            .save(&mut storage, (b"owner", b"spender2"), &3000)
            .unwrap();
        ALLOWANCE
            .save(&mut storage, (b"owner2", b"spender"), &5000)
            .unwrap();

        // get all under one key
        let all: Vec<_> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(
            all,
            [
                (b"spender".to_vec(), 1000_u64.to_borsh_vec().unwrap()),
                (b"spender2".to_vec(), 3000_u64.to_borsh_vec().unwrap())
            ]
        );

        // Or ranges between two items (even reverse)
        let all: Vec<_> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(
                &storage,
                Some(Bound::Exclusive(b"spender1")),
                Some(Bound::Inclusive(b"spender2")),
                Order::Descending,
            )
            .collect();
        assert_eq!(
            all,
            [(b"spender2".to_vec(), 3000_u64.to_borsh_vec().unwrap())]
        );
    }

    #[test]
    fn prefixed_range_raw_works() {
        // this is designed to look as much like a secondary index as possible
        // we want to query over a range of u32 for the first key and all subkeys
        const AGES: Map<(u32, Vec<u8>), u64> = Map::new("ages");

        let mut storage = MockStorage::new();
        AGES.save(&mut storage, (2, vec![1, 2, 3]), &123).unwrap();
        AGES.save(&mut storage, (3, vec![4, 5, 6]), &456).unwrap();
        AGES.save(&mut storage, (5, vec![7, 8, 9]), &789).unwrap();
        AGES.save(&mut storage, (5, vec![9, 8, 7]), &987).unwrap();
        AGES.save(&mut storage, (7, vec![20, 21, 22]), &2002)
            .unwrap();
        AGES.save(&mut storage, (8, vec![23, 24, 25]), &2332)
            .unwrap();

        // typical range under one prefix as a control
        let fives = AGES
            .prefix(5)
            .range_raw(&storage, None, None, Order::Ascending)
            .collect::<Vec<_>>();
        assert_eq!(
            fives,
            [
                (vec![7, 8, 9], 789_u64.to_borsh_vec().unwrap()),
                (vec![9, 8, 7], 987_u64.to_borsh_vec().unwrap())
            ]
        );

        // using inclusive bounds both sides
        let include = AGES
            .prefix_range_raw(
                &storage,
                Some(PrefixBound::Inclusive(3u32)),
                Some(PrefixBound::Inclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| r.1.deserialize_borsh().unwrap())
            .collect::<Vec<u64>>();
        assert_eq!(include, [456, 789, 987, 2002]);

        // using exclusive bounds both sides
        let exclude = AGES
            .prefix_range_raw(
                &storage,
                Some(PrefixBound::Exclusive(3u32)),
                Some(PrefixBound::Exclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| r.1.deserialize_borsh().unwrap())
            .collect::<Vec<u64>>();
        assert_eq!(exclude, [789, 987]);

        // using inclusive in descending
        let include = AGES
            .prefix_range_raw(
                &storage,
                Some(PrefixBound::Inclusive(3u32)),
                Some(PrefixBound::Inclusive(5u32)),
                Order::Descending,
            )
            .map(|r| r.1.deserialize_borsh().unwrap())
            .collect::<Vec<u64>>();
        assert_eq!(include, [987, 789, 456]);

        // using exclusive in descending
        let include = AGES
            .prefix_range_raw(
                &storage,
                Some(PrefixBound::Exclusive(2u32)),
                Some(PrefixBound::Exclusive(5u32)),
                Order::Descending,
            )
            .map(|r| r.1.deserialize_borsh().unwrap())
            .collect::<Vec<u64>>();
        assert_eq!(include, [456]);
    }

    #[test]
    fn prefixed_range_works() {
        // this is designed to look as much like a secondary index as possible
        // we want to query over a range of u32 for the first key and all subkeys
        const AGES: Map<(u32, &str), u64> = Map::new("ages");

        let mut storage = MockStorage::new();
        AGES.save(&mut storage, (2, "123"), &123).unwrap();
        AGES.save(&mut storage, (3, "456"), &456).unwrap();
        AGES.save(&mut storage, (5, "789"), &789).unwrap();
        AGES.save(&mut storage, (5, "987"), &987).unwrap();
        AGES.save(&mut storage, (7, "202122"), &2002).unwrap();
        AGES.save(&mut storage, (8, "232425"), &2332).unwrap();

        // typical range under one prefix as a control
        let fives = AGES
            .prefix(5)
            .range(&storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(fives, [("789".to_string(), 789), ("987".to_string(), 987)]);

        // using inclusive bounds both sides
        let include = AGES
            .prefix_range(
                &storage,
                Some(PrefixBound::Inclusive(3u32)),
                Some(PrefixBound::Inclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include, [456, 789, 987, 2002]);

        // using exclusive bounds both sides
        let exclude = AGES
            .prefix_range(
                &storage,
                Some(PrefixBound::Exclusive(3u32)),
                Some(PrefixBound::Exclusive(7u32)),
                Order::Ascending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(exclude, [789, 987]);

        // using inclusive in descending
        let include = AGES
            .prefix_range(
                &storage,
                Some(PrefixBound::Inclusive(3u32)),
                Some(PrefixBound::Inclusive(5u32)),
                Order::Descending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include, [987, 789, 456]);

        // using exclusive in descending
        let include = AGES
            .prefix_range(
                &storage,
                Some(PrefixBound::Exclusive(2u32)),
                Some(PrefixBound::Exclusive(5u32)),
                Order::Descending,
            )
            .map(|r| r.map(|(_, v)| v))
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include, [456]);
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
            Some(Bound::Inclusive("key0")),
            Some(Bound::Exclusive("key3")),
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
