use {
    crate::{Borsh, Bound, Key, PathBuf, Prefix, PrefixBound, Prefixer},
    grug_types::{Empty, Order, StdResult, Storage},
    std::marker::PhantomData,
};

/// Mimic the behavior of HashSet or BTreeSet.
///
/// Internally, this is basicaly a `Map<T, Empty>`.
///
/// We explicitly use Borsh here, because there's no benefit using any other
/// encoding scheme.
pub struct Set<'a, T> {
    pub(crate) namespace: &'a [u8],
    item: PhantomData<T>,
}

impl<'a, T> Set<'a, T> {
    pub const fn new(namespace: &'a str) -> Self {
        Self {
            namespace: namespace.as_bytes(),
            item: PhantomData,
        }
    }
}

impl<'a, T> Set<'a, T>
where
    T: Key,
{
    // ---------------------- methods for single entries -----------------------

    fn path(&self, item: T) -> PathBuf<Empty> {
        let mut raw_keys = item.raw_keys();
        let last_raw_key = raw_keys.pop();
        PathBuf::<Empty, Borsh>::new(self.namespace, &raw_keys, last_raw_key.as_ref())
    }

    pub fn has(&self, storage: &dyn Storage, item: T) -> bool {
        self.path(item).as_path().exists(storage)
    }

    pub fn insert(&self, storage: &mut dyn Storage, item: T) -> StdResult<()> {
        self.path(item).as_path().save(storage, &Empty {})
    }

    pub fn remove(&self, storage: &mut dyn Storage, item: T) {
        self.path(item).as_path().remove(storage)
    }

    // -------------------- iteration methods (full bound) ---------------------

    fn no_prefix(&self) -> Prefix<T, Empty> {
        Prefix::new(self.namespace, &[])
    }

    pub fn is_empty(&self, storage: &dyn Storage) -> bool {
        self.range_raw(storage, None, None, Order::Ascending)
            .next()
            .is_none()
    }

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

    pub fn prefix(&self, prefix: T::Prefix) -> Prefix<T::Suffix, Empty> {
        Prefix::new(self.namespace, &prefix.raw_prefixes())
    }

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
