#[cfg(feature = "metrics")]
use {crate::MetricsIterExt, std::time::Instant};
use {
    crate::{Batch, Op, Order, Record, Storage},
    std::{
        cmp::Ordering,
        iter::{self, Peekable},
        mem,
        ops::Bound,
    },
};

#[cfg(feature = "metrics")]
const BUFFER_LABEL: &str = "grug.types.buffer.duration";

/// A key-value storage with an in-memory write buffer.
///
/// Adapted from cw-multi-test:
/// <https://github.com/CosmWasm/cw-multi-test/blob/v0.19.0/src/transactions.rs#L170-L253>
#[derive(Clone)]
pub struct Buffer<S> {
    base: S,
    pending: Batch,
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    name: &'static str,
}

impl<S> Buffer<S> {
    /// Create a new buffer storage with an optional write batch.
    pub fn new(base: S, pending: Option<Batch>, name: &'static str) -> Self {
        Self {
            base,
            pending: pending.unwrap_or_default(),
            name,
        }
    }

    /// Create a new buffer, without a name for metrics.
    /// A default name "unknown" is used.
    pub fn new_unnamed(base: S, pending: Option<Batch>) -> Self {
        Self::new(base, pending, "unknown")
    }

    /// Comsume self, do not flush, just return the underlying store and the
    /// pending ops.
    pub fn disassemble(self) -> (S, Batch) {
        (self.base, self.pending)
    }
}

impl<S> Buffer<S>
where
    S: Storage,
{
    /// Flush pending ops to the underlying store.
    pub fn commit(&mut self) {
        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        let pending = mem::take(&mut self.pending);

        self.base.flush(pending);

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => "commit")
                .record(duration.elapsed().as_secs_f64());
        }
    }

    /// Consume self, flush pending ops to the underlying store, return the
    /// underlying store.
    pub fn consume(mut self) -> S {
        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        self.base.flush(self.pending);

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => "consume")
                .record(duration.elapsed().as_secs_f64());
        }

        self.base
    }

    fn scan_as<'a, I>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
        base: Box<dyn Iterator<Item = I> + 'a>,
        _operation: &'static str,
    ) -> Box<dyn Iterator<Item = I> + 'a>
    where
        I: AsKey + 'a,
    {
        if let (Some(min), Some(max)) = (min, max)
            && min > max
        {
            return Box::new(iter::empty());
        }

        let min = min.map_or(Bound::Unbounded, |bytes| Bound::Included(bytes.to_vec()));
        let max = max.map_or(Bound::Unbounded, |bytes| Bound::Excluded(bytes.to_vec()));

        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        let pending_raw = self.pending.range((min, max));

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => _operation)
                .record(duration.elapsed().as_secs_f64());
        }

        let pending: Box<dyn Iterator<Item = _>> = match order {
            Order::Ascending => Box::new(pending_raw),
            Order::Descending => Box::new(pending_raw.rev()),
        };

        #[cfg(feature = "metrics")]
        let pending =
            pending.with_metrics(BUFFER_LABEL, [("operation", "next"), ("name", self.name)]);

        Box::new(Merged::new(base, pending, order))
    }
}

impl<S> Storage for Buffer<S>
where
    S: Storage + Clone,
{
    fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        let pending = self.pending.get(key);

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => "read")
                .record(duration.elapsed().as_secs_f64());
        }

        match pending {
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
        self.scan_as(
            min,
            max,
            order,
            Box::new(self.base.scan(min, max, order)),
            "scan",
        )
    }

    fn scan_keys<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        self.scan_as(
            min,
            max,
            order,
            Box::new(self.base.scan_keys(min, max, order)),
            "scan_keys",
        )
    }

    fn scan_values<'a>(
        &'a self,
        min: Option<&[u8]>,
        max: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Vec<u8>> + 'a> {
        Box::new(
            self.scan_as(
                min,
                max,
                order,
                // Is not possible to use `base.scan_value`.
                // We need to have the key for compare with pending in `Merged`.
                Box::new(self.base.scan(min, max, order)),
                "scan_values",
            )
            .map(|(_, v)| v),
        )
    }

    fn write(&mut self, key: &[u8], value: &[u8]) {
        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        self.pending
            .insert(key.to_vec(), Op::Insert(value.to_vec()));

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => "write")
                .record(duration.elapsed().as_secs_f64());
        }
    }

    fn remove(&mut self, key: &[u8]) {
        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        self.pending.insert(key.to_vec(), Op::Delete);

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => "remove")
                .record(duration.elapsed().as_secs_f64());
        }
    }

    fn remove_range(&mut self, min: Option<&[u8]>, max: Option<&[u8]>) {
        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        // Find all keys within the bounds and mark them all as to be deleted.
        //
        // We use `self.scan_keys` here, which scans both the base and pending.
        //
        // We have to collect the iterator, because the iterator holds an
        // immutable reference to `self`, but `self.pending.extend` requires a
        // mutable reference, which can't coexist.
        let deletes = self
            .scan_keys(min, max, Order::Ascending)
            .map(|key| (key, Op::Delete))
            .collect::<Vec<_>>();

        self.pending.extend(deletes);

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => "remove_range")
                .record(duration.elapsed().as_secs_f64());
        }
    }

    fn flush(&mut self, batch: Batch) {
        // When we do `a.extend(b)`, while `a` and `b` have common keys, the
        // values in `b` are chosen. This is exactly what we want.
        #[cfg(feature = "metrics")]
        let duration = Instant::now();

        self.pending.extend(batch);

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(BUFFER_LABEL, "name" => self.name, "operation" => "flush")
                .record(duration.elapsed().as_secs_f64());
        }
    }
}

struct Merged<'a, B, P, I>
where
    B: Iterator<Item = I>,
    P: Iterator<Item = (&'a Vec<u8>, &'a Op)>,
    I: AsKey,
{
    base: Peekable<B>,
    pending: Peekable<P>,
    order: Order,
}

impl<'a, B, P, I> Merged<'a, B, P, I>
where
    B: Iterator<Item = I>,
    P: Iterator<Item = (&'a Vec<u8>, &'a Op)>,
    I: AsKey,
{
    pub fn new(base: B, pending: P, order: Order) -> Self {
        Self {
            base: base.peekable(),
            pending: pending.peekable(),
            order,
        }
    }

    /// Consume the next pending entry.
    ///
    /// Returns `None` both when the entry is a deletion — which yields no item
    /// and leaves the caller to keep looking — and when there are no pending
    /// entries left. The caller distinguishes the two by re-inspecting the
    /// iterators, so both cases are handled by simply continuing the loop.
    fn take_pending(&mut self) -> Option<I> {
        let (key, op) = self.pending.next()?;

        match op {
            Op::Insert(value) => Some(I::from_key_value(key, value)),
            Op::Delete => None,
        }
    }
}

impl<'a, B, P, I> Iterator for Merged<'a, B, P, I>
where
    B: Iterator<Item = I>,
    P: Iterator<Item = (&'a Vec<u8>, &'a Op)>,
    I: AsKey,
{
    type Item = I;

    fn next(&mut self) -> Option<Self::Item> {
        // Loop rather than recurse on deleted keys. A range over a region where
        // many contiguous keys have been deleted in the same buffer — a mass
        // prune, say — would otherwise consume one stack frame per deletion and
        // overflow the stack.
        loop {
            let item = match (self.base.peek(), self.pending.peek()) {
                (Some(i), Some((pending_key, _))) => {
                    let ordering_raw = i.as_key().cmp(pending_key);
                    let ordering = match self.order {
                        Order::Ascending => ordering_raw,
                        Order::Descending => ordering_raw.reverse(),
                    };

                    match ordering {
                        Ordering::Less => return self.base.next(),
                        Ordering::Equal => {
                            // The pending entry shadows the base one, so drop
                            // the latter whichever way the former resolves.
                            self.base.next();
                            self.take_pending()
                        },
                        Ordering::Greater => self.take_pending(),
                    }
                },
                (None, Some(_)) => self.take_pending(),
                (Some(_), None) => return self.base.next(),
                (None, None) => return None,
            };

            // A deleted key yields nothing; both iterators have advanced, so
            // keep going. Every path through the loop consumes at least one
            // entry, so this terminates.
            if item.is_some() {
                return item;
            }
        }
    }
}

/// A trait that represent a Iterator::Item that represent a key.
trait AsKey {
    fn from_key_value(key: &[u8], value: &[u8]) -> Self;

    fn as_key(&self) -> &[u8];
}

impl AsKey for Vec<u8> {
    fn from_key_value(key: &[u8], _value: &[u8]) -> Self {
        key.to_vec()
    }

    fn as_key(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsKey for Record {
    fn from_key_value(key: &[u8], value: &[u8]) -> Self {
        (key.to_vec(), value.to_vec())
    }

    fn as_key(&self) -> &[u8] {
        self.0.as_slice()
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {super::*, crate::MockStorage};

    // Illustration of this test case:
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

        let mut buffer = Buffer::new_unnamed(base, None);
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

    /// Iterating across a long run of deleted keys must cost no stack, so that
    /// wiping a large map and then reading it back stays within the thread's
    /// stack — worker threads get a couple of megabytes, far less than one
    /// frame per deleted key would need.
    ///
    /// Run on a thread with a deliberately small stack so that a per-deletion
    /// frame cost fails here rather than only at production scale.
    #[test]
    fn iterating_over_many_deletions_is_not_recursive() {
        const KEYS: u32 = 200_000;
        const STACK_SIZE: usize = 256 * 1024;

        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut base = MockStorage::new();

                for i in 0..KEYS {
                    base.write(&i.to_be_bytes(), &[1]);
                }

                let mut buffer = Buffer::new_unnamed(base, None);

                // Delete everything but the final key, so reaching it has to
                // step over the whole run of tombstones.
                for i in 0..KEYS - 1 {
                    buffer.remove(&i.to_be_bytes());
                }

                let records = collect_records(&buffer, Order::Ascending);

                assert_eq!(records, vec![((KEYS - 1).to_be_bytes().to_vec(), vec![1])]);

                // The same holds in reverse: the tombstones are then walked
                // before any surviving key is reached.
                buffer.remove(&(KEYS - 1).to_be_bytes());
                buffer.write(&[0], &[7]);

                assert_eq!(
                    collect_records(&buffer, Order::Descending),
                    vec![(vec![0], vec![7])]
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
