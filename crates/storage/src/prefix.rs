use {
    crate::{Borsh, Bound, Codec, Key, RawBound},
    grug_types::{
        concat, encode_length, extend_one_byte, increment_last_byte, nested_namespaces_with_key,
        trim, Order, Record, StdResult, Storage,
    },
    std::{borrow::Cow, marker::PhantomData},
};

pub struct Prefix<K, T, C: Codec<T> = Borsh> {
    prefix: Vec<u8>,
    suffix: PhantomData<K>,
    data: PhantomData<T>,
    codec: PhantomData<C>,
}

impl<K, T, C> Prefix<K, T, C>
where
    C: Codec<T>,
{
    pub fn new(namespace: &[u8], prefixes: &[Cow<[u8]>]) -> Self {
        Self {
            prefix: nested_namespaces_with_key(
                Some(namespace),
                prefixes,
                <Option<&Cow<[u8]>>>::None,
            ),
            suffix: PhantomData,
            data: PhantomData,
            codec: PhantomData,
        }
    }
}

impl<K, T, C> Prefix<K, T, C>
where
    K: Key,
    C: Codec<T>,
{
    pub fn append(mut self, prefix: K::Prefix) -> Prefix<K::Suffix, T, C> {
        for key_elem in prefix.raw_keys() {
            self.prefix.extend(encode_length(&key_elem));
            self.prefix.extend(key_elem.as_ref());
        }

        Prefix {
            prefix: self.prefix,
            suffix: PhantomData,
            data: self.data,
            codec: self.codec,
        }
    }

    pub fn range_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        // compute start and end bounds
        // note that the store considers the start bounds as inclusive, and end
        // bound as exclusive (see the Storage trait)
        let (min, max) = range_bounds(&self.prefix, min, max);

        // need to make a clone of self.prefix and move it into the closure,
        // so that the iterator can live longer than &self.
        let prefix = self.prefix.clone();
        let iter = storage
            .scan(Some(&min), Some(&max), order)
            .map(move |(k, v)| {
                debug_assert_eq!(&k[0..prefix.len()], prefix, "prefix mispatch");
                (trim(&prefix, &k), v)
            });

        Box::new(iter)
    }

    pub fn range_raw_sized<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
        size: u32,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let (min, max) = range_bounds(&self.prefix, min, max);
        let prefix = self.prefix.clone();

        let iter = storage
            .scan_sized(Some(&min), Some(&max), order, size)
            .map(move |(k, v)| {
                debug_assert_eq!(&k[0..prefix.len()], prefix, "prefix mispatch");
                (trim(&prefix, &k), v)
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
                let key = K::deserialize(&key_raw)?;
                let value = C::decode(&value_raw)?;
                Ok((key, value))
            });

        Box::new(iter)
    }

    pub fn range_sized<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
        size: u32,
    ) -> Box<dyn Iterator<Item = StdResult<(K::Output, T)>> + 'a> {
        let iter =
            self.range_raw_sized(storage, min, max, order, size)
                .map(|(key_raw, value_raw)| {
                    let key = K::deserialize(&key_raw)?;
                    let value = C::decode(&value_raw)?;
                    Ok((key, value))
                });

        Box::new(iter)
    }

    pub fn keys_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = range_bounds(&self.prefix, min, max);
        let prefix = self.prefix.clone();
        let iter = storage
            .scan_keys(Some(&min), Some(&max), order)
            .map(move |k| {
                debug_assert_eq!(&k[0..prefix.len()], prefix, "prefix mispatch");
                trim(&prefix, &k)
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
        let (min, max) = range_bounds(&self.prefix, min, max);
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
            .map(|key_raw| K::deserialize(&key_raw));

        Box::new(iter)
    }

    pub fn values_raw<'a>(
        &self,
        storage: &'a dyn Storage,
        min: Option<Bound<K>>,
        max: Option<Bound<K>>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        let (min, max) = range_bounds(&self.prefix, min, max);
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

    pub fn clear(&self, storage: &mut dyn Storage, min: Option<Bound<K>>, max: Option<Bound<K>>) {
        let (min, max) = range_bounds(&self.prefix, min, max);
        storage.remove_range(Some(&min), Some(&max))
    }
}

fn range_bounds<K: Key>(
    prefix: &[u8],
    min: Option<Bound<K>>,
    max: Option<Bound<K>>,
) -> (Vec<u8>, Vec<u8>) {
    let min = match min.map(RawBound::from) {
        None => prefix.to_vec(),
        Some(RawBound::Inclusive(k)) => concat(prefix, &k),
        Some(RawBound::Exclusive(k)) => extend_one_byte(concat(prefix, &k)),
    };
    let max = match max.map(RawBound::from) {
        None => increment_last_byte(prefix.to_vec()),
        Some(RawBound::Inclusive(k)) => extend_one_byte(concat(prefix, &k)),
        Some(RawBound::Exclusive(k)) => concat(prefix, &k),
    };

    (min, max)
}
