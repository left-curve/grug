use {
    crate::{Borsh, Bound, Codec, Key, KeysIterator, PathBuf, Prefix, PrefixBound, Prefixer},
    grug_types::{Empty, Order, StdResult, Storage},
    std::{borrow::Cow, marker::PhantomData},
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

impl<'a, T, C> Set<'a, T, C>
where
    T: Key,
    C: Codec<Empty>,
{
    fn path_raw(&self, key_raw: &[u8]) -> PathBuf<Empty, Borsh> {
        PathBuf::new(self.namespace, &[], Some(&Cow::Borrowed(key_raw)))
    }

    fn path(&self, item: T) -> PathBuf<Empty, Borsh> {
        let mut raw_keys = item.raw_keys();
        let last_raw_key = raw_keys.pop();
        PathBuf::new(self.namespace, &raw_keys, last_raw_key.as_ref())
    }

    fn no_prefix(&self) -> Prefix<T, Empty, C> {
        Prefix::new(self.namespace, &[])
    }

    pub fn prefix(&self, prefix: T::Prefix) -> Prefix<T::Suffix, Empty, C, KeysIterator> {
        Prefix::new(self.namespace, &prefix.raw_prefixes())
    }

    pub fn is_empty(&self, storage: &dyn Storage) -> bool {
        self.range_raw(storage, None, None, Order::Ascending)
            .next()
            .is_none()
    }

    // ---------------------- methods for single entries -----------------------

    pub fn has_raw(&self, storage: &dyn Storage, item_raw: &[u8]) -> bool {
        self.path_raw(item_raw).as_path().exists(storage)
    }

    pub fn has(&self, storage: &dyn Storage, item: T) -> bool {
        self.path(item).as_path().exists(storage)
    }

    /// Using this function is not recommended. If the item isn't properly
    /// serialized, later when you read the data, it will fail to deserialize
    /// and error.
    ///
    /// We prefix the function name with the word "unsafe" to highlight this.
    pub fn unsafe_insert_raw(&self, storage: &mut dyn Storage, item_raw: &[u8]) {
        // `Empty` serializes to empty bytes when using borsh.
        self.path_raw(item_raw).as_path().save_raw(storage, &[])
    }

    pub fn insert(&self, storage: &mut dyn Storage, item: T) -> StdResult<()> {
        self.path(item).as_path().save(storage, &Empty {})
    }

    pub fn remove_raw(&self, storage: &mut dyn Storage, item_raw: &[u8]) {
        self.path_raw(item_raw).as_path().remove(storage)
    }

    pub fn remove(&self, storage: &mut dyn Storage, item: T) {
        self.path(item).as_path().remove(storage)
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

#[cfg(test)]
mod tests {
    use {
        super::Set,
        crate::{Bound, Codec, Key, PrefixBound, Prefixer},
        grug_types::{concat, Dec128, Empty, MockStorage, NumberConst, Order, StdResult, Storage},
        std::str::FromStr,
    };

    const SINGLE: Set<&[u8]> = Set::new("single");

    const DOUBLE: Set<(Dec128, &str)> = Set::new("double");

    trait SetHelper {
        type T;
        fn all_raw(&self, storage: &mut dyn Storage) -> Vec<Vec<u8>>;
        fn all(&self, storage: &mut dyn Storage) -> StdResult<Vec<Self::T>>;
    }

    impl<'a, T, C> SetHelper for Set<'a, T, C>
    where
        T: Key,
        C: Codec<Empty>,
    {
        type T = <T as Key>::Output;

        fn all_raw(&self, storage: &mut dyn Storage) -> Vec<Vec<u8>> {
            self.range_raw(storage, None, None, Order::Ascending)
                .collect()
        }

        fn all(&self, storage: &mut dyn Storage) -> StdResult<Vec<Self::T>> {
            self.range(storage, None, None, Order::Ascending).collect()
        }
    }

    #[test]
    fn save_has_remove() -> StdResult<()> {
        let storage = &mut MockStorage::new();
        SINGLE.insert(storage, b"hello")?;

        assert!(SINGLE.has(storage, b"hello"));
        assert!(!SINGLE.has(storage, b"world"));

        DOUBLE.insert(storage, (Dec128::ONE, "world"))?;
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

        Ok(())
    }

    #[test]
    fn clear() -> StdResult<()> {
        let storage = &mut MockStorage::new();

        assert!(SINGLE.is_empty(storage));
        assert!(DOUBLE.is_empty(storage));

        for i in 0..100_u32 {
            SINGLE.insert(storage, &concat(b"foo", &i.joined_prefix()))?;
            DOUBLE.insert(storage, (Dec128::ONE, "bar"))?;
        }

        assert!(!SINGLE.is_empty(storage));
        assert!(!DOUBLE.is_empty(storage));

        // Min bound
        SINGLE.clear(
            storage,
            Some(Bound::inclusive(
                concat(b"foo", &70_u32.joined_prefix()).as_slice(),
            )),
            None,
        );

        assert_eq!(SINGLE.all_raw(storage).len(), 70);

        // Max bound
        SINGLE.clear(
            storage,
            None,
            Some(Bound::exclusive(
                concat(b"foo", &30_u32.joined_prefix()).as_slice(),
            )),
        );

        let all = SINGLE.all(storage)?;

        assert_eq!(all.len(), 40);
        assert_eq!(all[0], concat(b"foo", &30_u32.joined_prefix()));
        assert_eq!(all[39], concat(b"foo", &69_u32.joined_prefix()));

        // Max Min bound
        SINGLE.clear(
            storage,
            Some(Bound::inclusive(
                concat(b"foo", &40_u32.joined_prefix()).as_slice(),
            )),
            Some(Bound::exclusive(
                concat(b"foo", &50_u32.joined_prefix()).as_slice(),
            )),
        );

        let all = SINGLE.all(storage)?;

        assert_eq!(all.len(), 30);
        assert_eq!(all[0], concat(b"foo", &30_u32.joined_prefix()));
        assert_eq!(all[9], concat(b"foo", &39_u32.joined_prefix()));
        assert_eq!(all[10], concat(b"foo", &50_u32.joined_prefix()));
        assert_eq!(all[29], concat(b"foo", &69_u32.joined_prefix()));

        // Clear all
        SINGLE.clear(storage, None, None);

        assert_eq!(SINGLE.all(storage)?.len(), 0);

        Ok(())
    }

    #[test]
    fn range() -> StdResult<()> {
        let storage = &mut MockStorage::new();

        for i in 0..100_u32 {
            SINGLE.insert(storage, &concat(b"foo", &i.joined_prefix()))?;
        }

        // No bound
        {
            let data = SINGLE.all(storage)?;

            assert_eq!(data.len(), 100);
            assert_eq!(data[0], concat(b"foo", &0_u32.joined_prefix()));
            assert_eq!(data[99], concat(b"foo", &99_u32.joined_prefix()));
        }

        // Min bound
        {
            let data = SINGLE
                .range(
                    storage,
                    Some(Bound::inclusive(
                        concat(b"foo", &70_u32.joined_prefix()).as_slice(),
                    )),
                    None,
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()?;

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
                    Some(Bound::exclusive(
                        concat(b"foo", &30_u32.joined_prefix()).as_slice(),
                    )),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(data.len(), 30);
            assert_eq!(data[0], concat(b"foo", &0_u32.joined_prefix()));
            assert_eq!(data[29], concat(b"foo", &29_u32.joined_prefix()));
        }

        // Max Min bound
        {
            let data = SINGLE
                .range(
                    storage,
                    Some(Bound::inclusive(
                        concat(b"foo", &40_u32.joined_prefix()).as_slice(),
                    )),
                    Some(Bound::exclusive(
                        concat(b"foo", &50_u32.joined_prefix()).as_slice(),
                    )),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(data.len(), 10);
            assert_eq!(data[0], concat(b"foo", &40_u32.joined_prefix()));
            assert_eq!(data[9], concat(b"foo", &49_u32.joined_prefix()));
        }

        Ok(())
    }

    #[test]
    fn decimal_range() -> StdResult<()> {
        let storage = &mut MockStorage::new();

        for i in -50..50 {
            DOUBLE.insert(storage, (Dec128::from_str(&i.to_string())?, &i.to_string()))?;
        }

        let data = DOUBLE.all(storage)?;

        assert_eq!(data.len(), 100);

        for (index, val) in (-50..50).enumerate() {
            assert_eq!(
                data[index],
                (Dec128::from_str(&val.to_string()).unwrap(), val.to_string())
            );
        }

        Ok(())
    }

    #[test]
    fn prefix() -> StdResult<()> {
        let storage = &mut MockStorage::new();

        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "a"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "c"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "d"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "e"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-1.5")?, "a"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-1.5")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("0")?, "abcb"))?;
        DOUBLE.insert(storage, (Dec128::from_str("1")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("2")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("2")?, "a"))?;

        // No bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2")?)
                .keys(storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(val.len(), 5);
            assert_eq!(val[0], "a".to_string());
            assert_eq!(val[1], "b".to_string());
            assert_eq!(val[2], "c".to_string());
            assert_eq!(val[3], "d".to_string());
            assert_eq!(val[4], "e".to_string());
        }

        // Min bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2")?)
                .keys(storage, Some(Bound::inclusive("c")), None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(val.len(), 3);
            assert_eq!(val[0], "c".to_string());
            assert_eq!(val[1], "d".to_string());
            assert_eq!(val[2], "e".to_string());
        }

        // Max bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2")?)
                .keys(storage, None, Some(Bound::exclusive("d")), Order::Ascending)
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(val.len(), 3);
            assert_eq!(val[0], "a".to_string());
            assert_eq!(val[1], "b".to_string());
            assert_eq!(val[2], "c".to_string());
        }

        // Max Min bound
        {
            let val = DOUBLE
                .prefix(Dec128::from_str("-2")?)
                .keys(
                    storage,
                    Some(Bound::inclusive("b")),
                    Some(Bound::exclusive("d")),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(val.len(), 2);
            assert_eq!(val[0], "b".to_string());
            assert_eq!(val[1], "c".to_string());
        }
        Ok(())
    }

    #[test]
    fn prefix_range() -> StdResult<()> {
        let storage = &mut MockStorage::new();

        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "a"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "c"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "d"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-2")?, "e"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-1.5")?, "a"))?;
        DOUBLE.insert(storage, (Dec128::from_str("-1.5")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("0")?, "abcb"))?;
        DOUBLE.insert(storage, (Dec128::from_str("1")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("2")?, "b"))?;
        DOUBLE.insert(storage, (Dec128::from_str("2")?, "a"))?;

        // Min
        {
            let val = DOUBLE
                .prefix_range(
                    storage,
                    Some(PrefixBound::inclusive(Dec128::from_str("-1.5")?)),
                    None,
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(val.len(), 6);
            assert_eq!(val[0], (Dec128::from_str("-1.5")?, "a".to_string()));
            assert_eq!(val[1], (Dec128::from_str("-1.5")?, "b".to_string()));
            assert_eq!(val[2], (Dec128::from_str("0")?, "abcb".to_string()));
            assert_eq!(val[3], (Dec128::from_str("1")?, "b".to_string()));
            assert_eq!(val[4], (Dec128::from_str("2")?, "a".to_string()));
            assert_eq!(val[5], (Dec128::from_str("2")?, "b".to_string()));
        }

        // Max
        {
            let val = DOUBLE
                .prefix_range(
                    storage,
                    None,
                    Some(PrefixBound::exclusive(Dec128::from_str("0.5")?)),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(val.len(), 8);
            assert_eq!(val[0], (Dec128::from_str("-2")?, "a".to_string()));
            assert_eq!(val[1], (Dec128::from_str("-2")?, "b".to_string()));
            assert_eq!(val[2], (Dec128::from_str("-2")?, "c".to_string()));
            assert_eq!(val[3], (Dec128::from_str("-2")?, "d".to_string()));
            assert_eq!(val[4], (Dec128::from_str("-2")?, "e".to_string()));
            assert_eq!(val[5], (Dec128::from_str("-1.5")?, "a".to_string()));
            assert_eq!(val[6], (Dec128::from_str("-1.5")?, "b".to_string()));
            assert_eq!(val[7], (Dec128::from_str("0")?, "abcb".to_string()));
        }

        // Max Min
        {
            let val = DOUBLE
                .prefix_range(
                    storage,
                    Some(PrefixBound::inclusive(Dec128::from_str("-2")?)),
                    Some(PrefixBound::exclusive(Dec128::from_str("0")?)),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()?;

            assert_eq!(val.len(), 7);
            assert_eq!(val[0], (Dec128::from_str("-2")?, "a".to_string()));
            assert_eq!(val[1], (Dec128::from_str("-2")?, "b".to_string()));
            assert_eq!(val[2], (Dec128::from_str("-2")?, "c".to_string()));
            assert_eq!(val[3], (Dec128::from_str("-2")?, "d".to_string()));
            assert_eq!(val[4], (Dec128::from_str("-2")?, "e".to_string()));
            assert_eq!(val[5], (Dec128::from_str("-1.5")?, "a".to_string()));
            assert_eq!(val[6], (Dec128::from_str("-1.5")?, "b".to_string()));
        }

        Ok(())
    }
}
