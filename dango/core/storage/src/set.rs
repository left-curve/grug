use {
    crate::{Borsh, Codec, Path, Prefix, PrefixBound, Prefixer, PrimaryKey, RawKey},
    dango_primitives::{Bound, Empty, Order, StdResult, Storage},
    std::marker::PhantomData,
};

/// Mimic the behavior of HashSet or BTreeSet.
///
/// Internally, this is basicaly a `Map<T, Empty>`.
///
/// We explicitly use Borsh here, because there's no benefit using any other
/// encoding scheme.
pub struct Set<'a, T, C = Borsh>
where
    C: Codec<Empty>,
{
    pub(crate) namespace: &'a [u8],
    item: PhantomData<T>,
    codec: PhantomData<C>,
}

impl<'a, T, C> Set<'a, T, C>
where
    C: Codec<Empty>,
{
    pub const fn new(namespace: &'a str) -> Self {
        Self {
            namespace: namespace.as_bytes(),
            item: PhantomData,
            codec: PhantomData,
        }
    }
}

impl<T, C> Set<'_, T, C>
where
    T: PrimaryKey,
    C: Codec<Empty>,
{
    #[doc(hidden)]
    pub fn path_raw(&self, key_raw: &[u8]) -> Path<'_, Empty, Borsh> {
        Path::new(self.namespace, &[], Some(RawKey::Borrowed(key_raw)))
    }

    #[doc(hidden)]
    pub fn path(&self, item: T) -> Path<'_, Empty, Borsh> {
        let mut raw_keys = item.raw_keys();
        let last_raw_key = raw_keys.pop();
        Path::new(self.namespace, &raw_keys, last_raw_key)
    }

    #[doc(hidden)]
    pub fn no_prefix(&self) -> Prefix<T, Empty, C> {
        Prefix::new(self.namespace, &[])
    }

    pub fn prefix(&self, prefix: T::Prefix) -> Prefix<T::Suffix, Empty, C> {
        Prefix::new(self.namespace, &prefix.raw_prefixes())
    }

    pub fn is_empty(&self, storage: &dyn Storage) -> bool {
        self.no_prefix().is_empty(storage)
    }

    // ---------------------- methods for single entries -----------------------

    pub fn has_raw(&self, storage: &dyn Storage, item_raw: &[u8]) -> bool {
        self.path_raw(item_raw).exists(storage)
    }

    pub fn has(&self, storage: &dyn Storage, item: T) -> bool {
        self.path(item).exists(storage)
    }

    /// Using this function is not recommended. If the item isn't properly
    /// serialized, later when you read the data, it will fail to deserialize
    /// and error.
    ///
    /// We prefix the function name with the word "unsafe" to highlight this.
    pub fn unsafe_insert_raw(&self, storage: &mut dyn Storage, item_raw: &[u8]) {
        // `Empty` serializes to empty bytes when using borsh.
        self.path_raw(item_raw).save_raw(storage, &[])
    }

    pub fn insert(&self, storage: &mut dyn Storage, item: T) -> StdResult<()> {
        self.path(item).save(storage, &Empty {})
    }

    pub fn remove_raw(&self, storage: &mut dyn Storage, item_raw: &[u8]) {
        self.path_raw(item_raw).remove(storage)
    }

    pub fn remove(&self, storage: &mut dyn Storage, item: T) {
        self.path(item).remove(storage)
    }

    // -------------------- iteration methods (full bound) ---------------------

    pub fn range_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<T>>,
        max: Option<Bound<T>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.no_prefix().keys_raw(storage, min, max, order)
    }

    pub fn range<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<Bound<T>>,
        max: Option<Bound<T>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T::Output>> + 'b> {
        self.no_prefix().keys(storage, min, max, order)
    }

    pub fn clear(&self, storage: &mut dyn Storage, min: Option<Bound<T>>, max: Option<Bound<T>>) {
        self.no_prefix().clear(storage, min, max)
    }

    // ------------------- iteration methods (prefix bound) --------------------

    pub fn prefix_range_raw<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<T>>,
        max: Option<PrefixBound<T>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'b> {
        self.no_prefix().prefix_keys_raw(storage, min, max, order)
    }

    pub fn prefix_range<'b>(
        &self,
        storage: &'b dyn Storage,
        min: Option<PrefixBound<T>>,
        max: Option<PrefixBound<T>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T::Output>> + 'b> {
        self.no_prefix().prefix_keys(storage, min, max, order)
    }

    pub fn prefix_clear(
        &self,
        storage: &mut dyn Storage,
        min: Option<PrefixBound<T>>,
        max: Option<PrefixBound<T>>,
    ) {
        self.no_prefix().prefix_clear(storage, min, max)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{Codec, PrefixBound, Prefixer, PrimaryKey, Set},
        dango_math::{Dec128, NumberConst},
        dango_primitives::{Bound, Empty, MockStorage, Order, StdResult, Storage, concat},
        std::str::FromStr,
    };

    const SINGLE: Set<&[u8]> = Set::new("single");

    const DOUBLE: Set<(Dec128, &str)> = Set::new("double");

    trait SetExt {
        type T;

        // TODO: remove the allow dead code
        #[allow(dead_code)]
        fn all_raw(&self, storage: &mut dyn Storage) -> Vec<Vec<u8>>;

        fn all(&self, storage: &mut dyn Storage) -> StdResult<Vec<Self::T>>;
    }

    impl<T, C> SetExt for Set<'_, T, C>
    where
        T: PrimaryKey,
        C: Codec<Empty>,
    {
        type T = <T as PrimaryKey>::Output;

        fn all_raw(&self, storage: &mut dyn Storage) -> Vec<Vec<u8>> {
            self.range_raw(storage, None, None, Order::Ascending)
                .collect()
        }

        fn all(&self, storage: &mut dyn Storage) -> StdResult<Vec<Self::T>> {
            self.range(storage, None, None, Order::Ascending).collect()
        }
    }

    #[test]
    fn save_has_remove() {
        let storage = &mut MockStorage::new();
        SINGLE.insert(storage, b"hello").unwrap();

        assert!(SINGLE.has(storage, b"hello"));
        assert!(!SINGLE.has(storage, b"world"));

        DOUBLE.insert(storage, (Dec128::ONE, "world")).unwrap();
        assert!(DOUBLE.has(storage, (Dec128::ONE, "world")));
        assert!(!DOUBLE.has(storage, (Dec128::TEN, "world")));

        SINGLE.remove(storage, b"hello");
        assert!(!SINGLE.has(storage, b"hello"));

        DOUBLE.remove(storage, (Dec128::ONE, "world"));
        assert!(!DOUBLE.has(storage, (Dec128::ONE, "world")));

        SINGLE.unsafe_insert_raw(storage, b"foo");
        assert!(SINGLE.has_raw(storage, b"foo"));

        DOUBLE.unsafe_insert_raw(storage, b"foobar");
        assert!(DOUBLE.has_raw(storage, b"foobar"));

        SINGLE.remove_raw(storage, b"foo");
        assert!(!SINGLE.has_raw(storage, b"foo"));

        DOUBLE.remove_raw(storage, b"foobar");
        assert!(!DOUBLE.has_raw(storage, b"foobar"));
    }

    #[test]
    fn clear() {
        let storage = &mut MockStorage::new();

        assert!(SINGLE.is_empty(storage));
        assert!(DOUBLE.is_empty(storage));

        for i in 0..100_u32 {
            SINGLE
                .insert(storage, &concat(b"foo", &i.joined_prefix()))
                .unwrap();
            DOUBLE.insert(storage, (Dec128::ONE, "bar")).unwrap();
        }

        assert!(!SINGLE.is_empty(storage));
        assert!(!DOUBLE.is_empty(storage));

        // Min bound
        SINGLE.clear(
            storage,
            Some(Bound::Inclusive(
                concat(b"foo", &70_u32.joined_prefix()).as_slice(),
            )),
            None,
        );

        assert_eq!(SINGLE.all_raw(storage).len(), 70);

        // Max bound
        SINGLE.clear(
            storage,
            None,
            Some(Bound::Exclusive(
                concat(b"foo", &30_u32.joined_prefix()).as_slice(),
            )),
        );

        let all = SINGLE.all(storage).unwrap();

        assert_eq!(all.len(), 40);
        assert_eq!(all[0], concat(b"foo", &30_u32.joined_prefix()));
        assert_eq!(all[39], concat(b"foo", &69_u32.joined_prefix()));

        // Max Min bound
        SINGLE.clear(
            storage,
            Some(Bound::Inclusive(
                concat(b"foo", &40_u32.joined_prefix()).as_slice(),
            )),
            Some(Bound::Exclusive(
                concat(b"foo", &50_u32.joined_prefix()).as_slice(),
            )),
        );

        let all = SINGLE.all(storage).unwrap();

        assert_eq!(all.len(), 30);
        assert_eq!(all[0], concat(b"foo", &30_u32.joined_prefix()));
        assert_eq!(all[9], concat(b"foo", &39_u32.joined_prefix()));
        assert_eq!(all[10], concat(b"foo", &50_u32.joined_prefix()));
        assert_eq!(all[29], concat(b"foo", &69_u32.joined_prefix()));

        // Clear all
        SINGLE.clear(storage, None, None);

        assert_eq!(SINGLE.all(storage).unwrap().len(), 0);
    }

    #[test]
    fn range() {
        let storage = &mut MockStorage::new();

        for i in 0..100_u32 {
            SINGLE
                .insert(storage, &concat(b"foo", &i.joined_prefix()))
                .unwrap();
        }

        // No bound
        {
            let data = SINGLE.all(storage).unwrap();

            assert_eq!(data.len(), 100);
            assert_eq!(data[0], concat(b"foo", &0_u32.joined_prefix()));
            assert_eq!(data[99], concat(b"foo", &99_u32.joined_prefix()));
        }

        // Min bound
        {
            let data = SINGLE
                .range(
                    storage,
                    Some(Bound::Inclusive(
                        concat(b"foo", &70_u32.joined_prefix()).as_slice(),
                    )),
                    None,
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(data.len(), 30);
            assert_eq!(data[0], concat(b"foo", &70_u32.joined_prefix()));
            assert_eq!(data[29], concat(b"foo", &99_u32.joined_prefix()));
        }

        // Max bound
        {
            let data = SINGLE
                .range(
                    storage,
                    None,
                    Some(Bound::Exclusive(
                        concat(b"foo", &30_u32.joined_prefix()).as_slice(),
                    )),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(data.len(), 30);
            assert_eq!(data[0], concat(b"foo", &0_u32.joined_prefix()));
            assert_eq!(data[29], concat(b"foo", &29_u32.joined_prefix()));
        }

        // Max Min bound
        {
            let data = SINGLE
                .range(
                    storage,
                    Some(Bound::Inclusive(
                        concat(b"foo", &40_u32.joined_prefix()).as_slice(),
                    )),
                    Some(Bound::Exclusive(
                        concat(b"foo", &50_u32.joined_prefix()).as_slice(),
                    )),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(data.len(), 10);
            assert_eq!(data[0], concat(b"foo", &40_u32.joined_prefix()));
            assert_eq!(data[9], concat(b"foo", &49_u32.joined_prefix()));
        }
    }

    #[test]
    fn decimal_range() {
        let storage = &mut MockStorage::new();

        for i in -50..50 {
            DOUBLE
                .insert(
                    storage,
                    (Dec128::from_str(&i.to_string()).unwrap(), &i.to_string()),
                )
                .unwrap();
        }

        let data = DOUBLE.all(storage).unwrap();

        assert_eq!(data.len(), 100);

        for (index, val) in (-50..50).enumerate() {
            assert_eq!(
                data[index],
                (Dec128::from_str(&val.to_string()).unwrap(), val.to_string())
            );
        }
    }

    #[test]
    fn prefix() {
        let storage = &mut MockStorage::new();

        for (k, v) in [
            ("-2", "a"),
            ("-2", "b"),
            ("-2", "c"),
            ("-2", "d"),
            ("-2", "e"),
            ("-1.5", "a"),
            ("-1.5", "b"),
            ("0", "abab"),
            ("1", "b"),
            ("2", "b"),
            ("2", "a"),
        ] {
            DOUBLE
                .insert(storage, (Dec128::from_str(k).unwrap(), v))
                .unwrap();
        }

        // No bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2").unwrap())
                .keys(storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();
            assert_eq!(val, ["a", "b", "c", "d", "e"]);
        }

        // Min bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2").unwrap())
                .keys(storage, Some(Bound::Inclusive("c")), None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();
            assert_eq!(val, ["c", "d", "e"])
        }

        // Max bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2").unwrap())
                .keys(storage, None, Some(Bound::Exclusive("d")), Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();
            assert_eq!(val, ["a", "b", "c"]);
        }

        // Max Min bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2").unwrap())
                .keys(
                    storage,
                    Some(Bound::Inclusive("b")),
                    Some(Bound::Exclusive("d")),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();
            assert_eq!(val, ["b", "c"]);
        }
    }

    #[test]
    fn prefix_range() {
        let storage = &mut MockStorage::new();

        for (k, v) in [
            ("-2", "a"),
            ("-2", "b"),
            ("-2", "c"),
            ("-2", "d"),
            ("-2", "e"),
            ("-1.5", "a"),
            ("-1.5", "b"),
            ("0", "abcb"),
            ("1", "b"),
            ("2", "b"),
            ("2", "a"),
        ] {
            DOUBLE
                .insert(storage, (Dec128::from_str(k).unwrap(), v))
                .unwrap();
        }

        // Min
        {
            let val = DOUBLE
                .prefix_range(
                    storage,
                    Some(PrefixBound::Inclusive(Dec128::from_str("-1.5").unwrap())),
                    None,
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                val,
                [
                    (Dec128::from_str("-1.5").unwrap(), "a".to_string()),
                    (Dec128::from_str("-1.5").unwrap(), "b".to_string()),
                    (Dec128::from_str("0").unwrap(), "abcb".to_string()),
                    (Dec128::from_str("1").unwrap(), "b".to_string()),
                    (Dec128::from_str("2").unwrap(), "a".to_string()),
                    (Dec128::from_str("2").unwrap(), "b".to_string())
                ]
            );
        }

        // Max
        {
            let val = DOUBLE
                .prefix_range(
                    storage,
                    None,
                    Some(PrefixBound::Exclusive(Dec128::from_str("0.5").unwrap())),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                val,
                [
                    (Dec128::from_str("-2").unwrap(), "a".to_string()),
                    (Dec128::from_str("-2").unwrap(), "b".to_string()),
                    (Dec128::from_str("-2").unwrap(), "c".to_string()),
                    (Dec128::from_str("-2").unwrap(), "d".to_string()),
                    (Dec128::from_str("-2").unwrap(), "e".to_string()),
                    (Dec128::from_str("-1.5").unwrap(), "a".to_string()),
                    (Dec128::from_str("-1.5").unwrap(), "b".to_string()),
                    (Dec128::from_str("0").unwrap(), "abcb".to_string())
                ]
            );
        }

        // Max Min
        {
            let val = DOUBLE
                .prefix_range(
                    storage,
                    Some(PrefixBound::Inclusive(Dec128::from_str("-2").unwrap())),
                    Some(PrefixBound::Exclusive(Dec128::from_str("0").unwrap())),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                val,
                [
                    (Dec128::from_str("-2").unwrap(), "a".to_string()),
                    (Dec128::from_str("-2").unwrap(), "b".to_string()),
                    (Dec128::from_str("-2").unwrap(), "c".to_string()),
                    (Dec128::from_str("-2").unwrap(), "d".to_string()),
                    (Dec128::from_str("-2").unwrap(), "e".to_string()),
                    (Dec128::from_str("-1.5").unwrap(), "a".to_string()),
                    (Dec128::from_str("-1.5").unwrap(), "b".to_string())
                ]
            );
        }
    }
}
