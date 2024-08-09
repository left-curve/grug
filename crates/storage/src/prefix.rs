use {
    crate::{Bound, Codec, Key, PrefixBound, Prefixer, RawBound},
    grug_types::{
        concat, encode_length, extend_one_byte, increment_last_byte, nested_namespaces_with_key,
        trim, Order, Record, StdResult, Storage,
    },
    std::{borrow::Cow, marker::PhantomData},
};

pub trait AsKeysIterator {}
pub trait AsRangeIterator {}

pub struct KeysIterator;
impl AsKeysIterator for KeysIterator {}

pub struct RangeIterator;
impl AsRangeIterator for RangeIterator {}
impl AsKeysIterator for RangeIterator {}

pub struct Prefix<K, T, C, I = RangeIterator>
where
    C: Codec<T>,
{
    namespace: Vec<u8>,
    phantom: PhantomData<(K, T, C, I)>,
}

impl<K, T, C, I> Prefix<K, T, C, I>
where
    C: Codec<T>,
{
    pub fn new(namespace: &[u8], prefixes: &[Cow<[u8]>]) -> Self {
        Self {
            namespace: nested_namespaces_with_key(
                Some(namespace),
                prefixes,
                <Option<&Cow<[u8]>>>::None,
            ),
            phantom: PhantomData,
        }
    }
}

impl<K, T, C, I> Prefix<K, T, C, I>
where
    K: Key,
    C: Codec<T>,
{
    pub fn append(mut self, prefix: K::Prefix) -> Prefix<K::Suffix, T, C> {
        for key_elem in prefix.raw_prefixes() {
            self.namespace.extend(encode_length(&key_elem));
            self.namespace.extend(key_elem.as_ref());
        }

        Prefix {
            namespace: self.namespace,
            phantom: PhantomData,
        }
    }

    // -------------------- iteration methods (full bound) ---------------------

    pub fn clear(&self, storage: &mut dyn Storage, min: Option<Bound<K>>, max: Option<Bound<K>>) {
        let (min, max) = range_bounds(&self.namespace, min, max);
        storage.remove_range(Some(&min), Some(&max))
    }

    // ------------------- iteration methods (prefix bound) --------------------

    pub fn prefix_clear(
        &self,
        storage: &mut dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
    ) {
        let (min, max) = range_prefix_bounds(&self.namespace, min, max);
        storage.remove_range(Some(&min), Some(&max))
    }
}

// --- Keys iterator ---
impl<K, T, C, I> Prefix<K, T, C, I>
where
    K: Key,
    C: Codec<T>,
    I: AsKeysIterator,
{
    pub fn is_empty(&self, storage: &dyn Storage) -> bool {
        self.keys_raw(storage, None, None, Order::Ascending)
            .next()
            .is_none()
    }

    pub fn keys_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = range_bounds(&self.namespace, min, max);
        let namespace = self.namespace.clone();
        let iter = storage
            .scan_keys(Some(&min), Some(&max), order)
            .map(move |k| {
                debug_assert_eq!(&k[0..namespace.len()], namespace, "namespace mispatch");
                trim(&namespace, &k)
            });

        Box::new(iter)
    }

    /// Iterate the raw primary keys under the given index value, without
    /// trimming the prefix (the whole key is returned).
    ///
    /// This is used internally for the indexed map.
    pub(crate) fn keys_raw_no_trimmer<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = range_bounds(&self.namespace, min, max);
        let iter = storage.scan_keys(Some(&min), Some(&max), order);

        Box::new(iter)
    }

    pub fn keys<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<K::Output>> + 'a> {
        let iter = self
            .keys_raw(storage, min, max, order)
            .map(|key_raw| K::from_slice(&key_raw));

        Box::new(iter)
    }

    pub fn prefix_keys_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = range_prefix_bounds(&self.namespace, min, max);
        let namespace = self.namespace.clone();
        let iter = storage
            .scan_keys(Some(&min), Some(&max), order)
            .map(move |k| {
                debug_assert_eq!(&k[0..namespace.len()], namespace, "namespace mispatch");
                trim(&namespace, &k)
            });

        Box::new(iter)
    }

    pub fn prefix_keys<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<K::Output>> + 'a> {
        let iter = self
            .prefix_keys_raw(storage, min, max, order)
            .map(|key_raw| K::from_slice(&key_raw));

        Box::new(iter)
    }
}

// --- Range iterator ---
impl<K, T, C, I> Prefix<K, T, C, I>
where
    K: Key,
    C: Codec<T>,
    I: AsRangeIterator,
{
    pub fn range_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        // Compute start and end bounds.
        // Note that the store considers the start bounds as inclusive, and end
        // bound as exclusive (see the Storage trait).
        let (min, max) = range_bounds(&self.namespace, min, max);

        // Need to make a clone of self.prefix and move it into the closure,
        // so that the iterator can live longer than `&self`.
        let namespace = self.namespace.clone();
        let iter = storage
            .scan(Some(&min), Some(&max), order)
            .map(move |(k, v)| {
                debug_assert_eq!(&k[0..namespace.len()], namespace, "namespace mispatch");
                (trim(&namespace, &k), v)
            });

        Box::new(iter)
    }

    pub fn range<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(K::Output, T)>> + 'a> {
        let iter = self
            .range_raw(storage, min, max, order)
            .map(|(key_raw, value_raw)| {
                let key = K::from_slice(&key_raw)?;
                let value = C::decode(&value_raw)?;
                Ok((key, value))
            });

        Box::new(iter)
    }

    pub fn values_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = range_bounds(&self.namespace, min, max);
        let iter = storage.scan_values(Some(&min), Some(&max), order);

        Box::new(iter)
    }

    pub fn values<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'a> {
        let iter = self
            .values_raw(storage, min, max, order)
            .map(|value_raw| C::decode(&value_raw));

        Box::new(iter)
    }

    pub fn prefix_range_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let (min, max) = range_prefix_bounds(&self.namespace, min, max);
        let namespace = self.namespace.clone();
        let iter = storage
            .scan(Some(&min), Some(&max), order)
            .map(move |(k, v)| {
                debug_assert_eq!(&k[0..namespace.len()], namespace, "namespace mispatch");
                (trim(&namespace, &k), v)
            });

        Box::new(iter)
    }

    pub fn prefix_range<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<(K::Output, T)>> + 'a> {
        let iter = self
            .prefix_range_raw(storage, min, max, order)
            .map(|(key_raw, value_raw)| {
                let key = K::from_slice(&key_raw)?;
                let value = C::decode(&value_raw)?;
                Ok((key, value))
            });

        Box::new(iter)
    }

    pub fn prefix_values_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = range_prefix_bounds(&self.namespace, min, max);
        let iter = storage.scan_values(Some(&min), Some(&max), order);

        Box::new(iter)
    }

    pub fn prefix_values<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<PrefixBound<K>>,
        max: Option<PrefixBound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = StdResult<T>> + 'a> {
        let iter = self
            .prefix_values_raw(storage, min, max, order)
            .map(|value_raw| C::decode(&value_raw));

        Box::new(iter)
    }
}

// ------

fn range_bounds<K>(
    namespace: &[u8],
    min: Option<Bound<K>>,
    max: Option<Bound<K>>,
) -> (Vec<u8>, Vec<u8>)
where
    K: Key,
{
    let min = match min.map(RawBound::from) {
        None => namespace.to_vec(),
        Some(RawBound::Inclusive(k)) => concat(namespace, &k),
        Some(RawBound::Exclusive(k)) => concat(namespace, &extend_one_byte(k)),
    };
    let max = match max.map(RawBound::from) {
        None => increment_last_byte(namespace.to_vec()),
        Some(RawBound::Inclusive(k)) => concat(namespace, &extend_one_byte(k)),
        Some(RawBound::Exclusive(k)) => concat(namespace, &k),
    };

    (min, max)
}

fn range_prefix_bounds<K>(
    namespace: &[u8],
    min: Option<PrefixBound<K>>,
    max: Option<PrefixBound<K>>,
) -> (Vec<u8>, Vec<u8>)
where
    K: Key,
{
    let min = match min.map(RawBound::from) {
        None => namespace.to_vec(),
        Some(RawBound::Inclusive(p)) => concat(namespace, &p),
        Some(RawBound::Exclusive(p)) => concat(namespace, &increment_last_byte(p)),
    };
    let max = match max.map(RawBound::from) {
        None => increment_last_byte(namespace.to_vec()),
        Some(RawBound::Inclusive(p)) => concat(namespace, &increment_last_byte(p)),
        Some(RawBound::Exclusive(p)) => concat(namespace, &p),
    };

    (min, max)
}

#[cfg(test)]
mod test {
    use {super::*, crate::Borsh, grug_types::MockStorage};

    #[test]
    fn ensure_proper_range_bounds() {
        let mut store = MockStorage::new();
        // manually create this - not testing nested prefixes here
        let prefix: Prefix<Vec<u8>, u64, Borsh> = Prefix {
            namespace: b"foo".to_vec(),
            phantom: PhantomData,
        };

        // set some data, we care about "foo" prefix
        store.write(b"foobar", b"1");
        store.write(b"foora", b"2");
        store.write(b"foozi", b"3");
        // these shouldn't match
        store.write(b"foply", b"100");
        store.write(b"font", b"200");

        let expected = vec![
            (b"bar".to_vec(), b"1".to_vec()),
            (b"ra".to_vec(), b"2".to_vec()),
            (b"zi".to_vec(), b"3".to_vec()),
        ];
        let expected_reversed: Vec<(Vec<u8>, Vec<u8>)> = expected.iter().rev().cloned().collect();

        // let's do the basic sanity check
        let res: Vec<_> = prefix
            .range_raw(&store, None, None, Order::Ascending)
            .collect();
        assert_eq!(&expected, &res);
        let res: Vec<_> = prefix
            .range_raw(&store, None, None, Order::Descending)
            .collect();
        assert_eq!(&expected_reversed, &res);

        // now let's check some ascending ranges
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                Some(Bound::inclusive(b"ra".to_vec())),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(&expected[1..], res.as_slice());
        // skip excluded
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                Some(Bound::exclusive(b"ra".to_vec())),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(&expected[2..], res.as_slice());
        // if we exclude something a little lower, we get matched
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                Some(Bound::exclusive(b"r".to_vec())),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(&expected[1..], res.as_slice());

        // now let's check some descending ranges
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                None,
                Some(Bound::inclusive(b"ra".to_vec())),
                Order::Descending,
            )
            .collect();
        assert_eq!(&expected_reversed[1..], res.as_slice());
        // skip excluded
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                None,
                Some(Bound::exclusive(b"ra".to_vec())),
                Order::Descending,
            )
            .collect();
        assert_eq!(&expected_reversed[2..], res.as_slice());
        // if we exclude something a little higher, we get matched
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                None,
                Some(Bound::exclusive(b"rb".to_vec())),
                Order::Descending,
            )
            .collect();
        assert_eq!(&expected_reversed[1..], res.as_slice());

        // now test when both sides are set
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                Some(Bound::inclusive(b"ra".to_vec())),
                Some(Bound::exclusive(b"zi".to_vec())),
                Order::Ascending,
            )
            .collect();
        assert_eq!(&expected[1..2], res.as_slice());
        // and descending
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                Some(Bound::inclusive(b"ra".to_vec())),
                Some(Bound::exclusive(b"zi".to_vec())),
                Order::Descending,
            )
            .collect();
        assert_eq!(&expected[1..2], res.as_slice());
        // Include both sides
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                Some(Bound::inclusive(b"ra".to_vec())),
                Some(Bound::inclusive(b"zi".to_vec())),
                Order::Descending,
            )
            .collect();
        assert_eq!(&expected_reversed[..2], res.as_slice());
        // Exclude both sides
        let res: Vec<_> = prefix
            .range_raw(
                &store,
                Some(Bound::exclusive(b"ra".to_vec())),
                Some(Bound::exclusive(b"zi".to_vec())),
                Order::Ascending,
            )
            .collect();
        assert_eq!(res.as_slice(), &[]);
    }

    // #[test]
    // fn prefix_debug() {
    //     let prefix: Prefix<String, String, Borsh> = Prefix::new(b"lol", &[Key::Val8([8; 1])]);
    //     assert_eq!(
    //         format!("{:?}", prefix),
    //         "Prefix { storage_prefix: [0, 3, 108, 111, 108, 0, 1, 8], .. }"
    //     );
    // }

    #[test]
    fn prefix_clear_limited() {
        let mut store = MockStorage::new();
        // manually create this - not testing nested prefixes here
        let prefix: Prefix<i32, u64, Borsh> = Prefix {
            namespace: b"foo".to_vec(),
            phantom: PhantomData,
        };

        // set some data, we care about "foo" prefix
        for i in 0..100i32 {
            let mut buf = "foo".joined_key();
            buf.extend_from_slice(&i.joined_key());
            store.write(&buf, b"1");
        }

        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            100
        );

        // clear with min bound
        prefix.clear(&mut store, None, Some(Bound::inclusive(20i32)));
        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            100 - 21
        );

        // clear with max bound
        prefix.clear(&mut store, Some(Bound::inclusive(50)), None);
        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            100 - 21 - 50
        );

        // clearing more than available should work
        prefix.clear(&mut store, None, Some(Bound::inclusive(200)));
        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            0
        );
    }

    #[test]
    fn prefix_clear_unlimited() {
        let mut store = MockStorage::new();
        let prefix: Prefix<Vec<u8>, u64, Borsh> = Prefix::new(b"foo", &[]);

        // set some data, we care about "foo" prefix
        for i in 0..1000u32 {
            store.write(&concat(&[0, 3], format!("foo{}", i).as_bytes()), b"1");
        }

        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            1000
        );

        // clearing all should work
        prefix.clear(&mut store, None, None);
        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            0
        );

        // set less data
        for i in 0..5u32 {
            store.write(&concat(&[0, 3], format!("foo{}", i).as_bytes()), b"1");
        }

        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            5
        );

        // clearing all should work
        prefix.clear(&mut store, None, None);
        assert_eq!(
            prefix.range(&store, None, None, Order::Ascending).count(),
            0
        );
    }

    #[test]
    fn is_empty_works() {
        let prefix: Prefix<Vec<u8>, u64, Borsh> = Prefix::new(b"foo", &[]);

        let mut storage = MockStorage::new();

        assert!(prefix.is_empty(&storage));

        storage.write(&concat(&[0, 3], b"fookey1"), b"1");
        storage.write(&concat(&[0, 3], b"fookey2"), b"2");

        assert!(!prefix.is_empty(&storage));
    }

    #[test]
    fn keys_raw_works() {
        let prefix: Prefix<Vec<u8>, u64, Borsh> = Prefix::new(b"foo", &[]);

        let mut storage = MockStorage::new();
        storage.write(&concat(&[0, 3], b"fookey1"), b"1");
        storage.write(&concat(&[0, 3], b"fookey2"), b"2");

        let keys: Vec<_> = prefix
            .keys_raw(&storage, None, None, Order::Ascending)
            .collect();
        assert_eq!(keys, vec![b"key1", b"key2"]);

        let keys: Vec<_> = prefix
            .keys_raw(
                &storage,
                Some(Bound::exclusive("key1")),
                None,
                Order::Ascending,
            )
            .collect();
        assert_eq!(keys, vec![b"key2"]);
    }
}
