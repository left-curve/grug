use {
    grug_types::{Batch, Op, Order, Record, Storage},
    std::{
        cmp::Ordering,
        iter::{self, Peekable},
        mem,
        ops::Bound,
    },
};

/// A key-value storage with an in-memory write buffer.
///
/// Adapted from cw-multi-test:
/// <https://github.com/CosmWasm/cw-multi-test/blob/v0.19.0/src/transactions.rs#L170-L253>
#[derive(Clone)]
pub struct Buffer<S: Clone> {
    base: S,
    pending: Batch,
}

impl<S: Clone> Buffer<S> {
    /// Create a new buffer storage with an optional write batch.
    pub fn new(base: S, pending: Option<Batch>) -> Self {
        Self {
            base,
            pending: pending.unwrap_or_default(),
        }
    }

    /// Comsume self, do not flush, just return the underlying store and the
    /// pending ops.
    pub fn disassemble(self) -> (S, Batch) {
        (self.base, self.pending)
    }
}

impl<S: Storage + Clone> Buffer<S> {
    /// Flush pending ops to the underlying store.
    pub fn commit(&mut self) {
        let pending = mem::take(&mut self.pending);
        self.base.flush(pending);
    }

    /// Consume self, flush pending ops to the underlying store, return the
    /// underlying store.
    pub fn consume(mut self) -> S {
        self.base.flush(self.pending);
        self.base
    }
}

impl<S: Storage + Clone> Storage for Buffer<S> {
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        match self.pending.get(key) {
            Some(Op::Insert(value)) => Some(value.clone()),
            Some(Op::Delete) => None,
            None => self.base.read(key),
        }
    }

    fn scan<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        if let (Some(min), Some(max)) = (min, max) {
            if min > max {
                return Box::new(iter::empty());
            }
        }

        let base = self.base.scan(min, max, order);

        let min = min.map_or(Bound::Unbounded, |bytes| Bound::Included(bytes.to_vec()));
        let max = max.map_or(Bound::Unbounded, |bytes| Bound::Excluded(bytes.to_vec()));
        let pending_raw = self.pending.range((min, max));
        let pending: Box<dyn Iterator<Item = _>> = match order {
            Order::Ascending => Box::new(pending_raw),
            Order::Descending => Box::new(pending_raw.rev()),
        };

        Box::new(Merged::new(base, pending, order))
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        // Currently we simply iterate both keys and values, and discard the
        // values. This isn't efficient.
        // TODO: optimize this
        Box::new(self.scan(min, max, order).map(|(k, _)| k))
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        Box::new(self.scan(min, max, order).map(|(_, v)| v))
    }

    fn write(&mut self, key: &[u8], value: &[u8]) {
        self.pending
            .insert(key.to_vec(), Op::Insert(value.to_vec()));
    }

    fn remove(&mut self, key: &[u8]) {
        self.pending.insert(key.to_vec(), Op::Delete);
    }

    fn remove_range(&mut self, min: Option<&[u8]>, max: Option<&[u8]>) {
        // Find all keys within the bounds and mark them all as to be deleted.
        // We use `self.scan_keys` here, which scans both the base and pending.
        // We have to collect the iterator, because the iterator holds an
        //
        // immutable reference to `self`, but `self.pending.extend` requires a
        // mutable reference, which can't coexist.
        let deletes = self
            .scan_keys(min, max, Order::Ascending)
            .map(|key| (key, Op::Delete))
            .collect::<Vec<_>>();
        self.pending.extend(deletes);
    }

    fn flush(&mut self, batch: Batch) {
        // if we do a.extend(b), while a and b have common keys, the values in b
        // are chosen. this is exactly what we want.
        self.pending.extend(batch);
    }

    fn scan_sized<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
        size: u32,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        // TODO: optimize this??
        Box::new(self.scan(min, max, order).take(size as usize))
    }
}

struct Merged<'a, B, P>
where
    B: Iterator<Item = Record>,
    P: Iterator<Item = (&'a Vec<u8>, &'a Op)>,
{
    base: Peekable<B>,
    pending: Peekable<P>,
    order: Order,
}

impl<'a, B, P> Merged<'a, B, P>
where
    B: Iterator<Item = Record>,
    P: Iterator<Item = (&'a Vec<u8>, &'a Op)>,
{
    pub fn new(base: B, pending: P, order: Order) -> Self {
        Self {
            base: base.peekable(),
            pending: pending.peekable(),
            order,
        }
    }

    fn take_pending(&mut self) -> Option<Record> {
        let (key, op) = self.pending.next()?;
        match op {
            Op::Insert(value) => Some((key.clone(), value.clone())),
            Op::Delete => self.next(),
        }
    }
}

impl<'a, B, P> Iterator for Merged<'a, B, P>
where
    B: Iterator<Item = Record>,
    P: Iterator<Item = (&'a Vec<u8>, &'a Op)>,
{
    type Item = Record;

    fn next(&mut self) -> Option<Self::Item> {
        match (self.base.peek(), self.pending.peek()) {
            (Some((base_key, _)), Some((pending_key, _))) => {
                let ordering_raw = base_key.cmp(pending_key);
                let ordering = match self.order {
                    Order::Ascending => ordering_raw,
                    Order::Descending => ordering_raw.reverse(),
                };

                match ordering {
                    Ordering::Less => self.base.next(),
                    Ordering::Equal => {
                        self.base.next();
                        self.take_pending()
                    },
                    Ordering::Greater => self.take_pending(),
                }
            },
            (None, Some(_)) => self.take_pending(),
            (Some(_), None) => self.base.next(),
            (None, None) => None,
        }
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {super::*, grug_types::MockStorage};

    // illustration of this test case:
    //
    // base    : 1 2 _ 4 5 6 7 _
    // pending :   D P _ _ P D 8  (P = put, D = delete)
    // merged  : 1 _ 3 4 5 6 _ 8
    fn make_test_case() -> (Buffer<MockStorage>, Vec<Record>) {
        let mut base = MockStorage::new();
        base.write(&[1], &[1]);
        base.write(&[2], &[2]);
        base.write(&[4], &[4]);
        base.write(&[5], &[5]);
        base.write(&[6], &[6]);
        base.write(&[7], &[7]);

        let mut buffer = Buffer::new(base, None);
        buffer.remove(&[2]);
        buffer.write(&[3], &[3]);
        buffer.write(&[6], &[255]);
        buffer.remove(&[7]);
        buffer.write(&[8], &[8]);

        let merged = vec![
            (vec![1], vec![1]),
            (vec![3], vec![3]),
            (vec![4], vec![4]),
            (vec![5], vec![5]),
            (vec![6], vec![255]),
            (vec![8], vec![8]),
        ];

        (buffer, merged)
    }

    fn collect_records(storage: &dyn Storage, order: Order) -> Vec<Record> {
        storage.scan(None, None, order).collect()
    }

    #[test]
    fn iterator_works() {
        let (buffer, mut merged) = make_test_case();
        assert_eq!(collect_records(&buffer, Order::Ascending), merged);

        merged.reverse();
        assert_eq!(collect_records(&buffer, Order::Descending), merged);
    }

    // TODO: add fuzz test
}
