use {super::GENESIS_HEIGHT, rangemap::RangeInclusiveSet};

/// The set of block heights present in the store, held as coalesced inclusive
/// ranges — one entry per contiguous stretch, so a million-block backfill or a
/// large live island collapses to a single range.
///
/// This is the **single owner of the store's topology math**: every `BlockStore`
/// impl (in-memory, RocksDB, ...) delegates the frontier/gap bookkeeping here,
/// so the stores themselves are thin I/O adapters with no duplicated logic. The
/// size is O(#gaps + 1), never O(#blocks), which is why a persistent store can
/// checkpoint it cheaply (`to_ranges`) and reload it in O(#ranges) at boot
/// (`from_ranges`) instead of scanning every stored height.
#[derive(Debug, Default, Clone)]
pub(crate) struct StoredRanges(RangeInclusiveSet<u64>);

impl StoredRanges {
    /// Rebuild from a checkpoint (the inverse of [`to_ranges`](Self::to_ranges)).
    pub(crate) fn from_ranges(ranges: &[(u64, u64)]) -> Self {
        let mut set = RangeInclusiveSet::new();
        for &(start, end) in ranges {
            set.insert(start..=end);
        }
        Self(set)
    }

    /// Mark one height as present, coalescing with any adjacent range, and
    /// report the new contiguous frontier **iff it advanced** as a result —
    /// `None` for a duplicate or an island above a gap. On a bulk-advance (a
    /// height that bridges the prefix to a stored run) the returned frontier can
    /// jump far past `height`.
    pub(crate) fn insert(&mut self, height: u64) -> Option<u64> {
        let before = self.contiguous_top();
        self.0.insert(height..=height);
        let after = self.contiguous_top();
        match (before, after) {
            (None, Some(top)) => Some(top),
            (Some(was), Some(top)) if top > was => Some(top),
            _ => None,
        }
    }

    /// Whether `height` is already present (drives the idempotent `put`).
    pub(crate) fn contains(&self, height: u64) -> bool {
        self.0.contains(&height)
    }

    /// The contiguous frontier: the top of the run anchored at `GENESIS_HEIGHT`,
    /// or `None` if genesis itself is not present (nothing is contiguous yet).
    pub(crate) fn contiguous_top(&self) -> Option<u64> {
        self.0.get(&GENESIS_HEIGHT).map(|range| *range.end())
    }

    /// The lowest missing range at or above `GENESIS_HEIGHT`, bounded above by
    /// the highest stored height — heights past the top are the not-yet-fetched
    /// future, not a hole. `None` when the stored prefix is gap-free.
    pub(crate) fn first_gap(&self) -> Option<(u64, u64)> {
        let top = *self.0.iter().next_back()?.end();
        self.0
            .gaps(&(GENESIS_HEIGHT..=top))
            .next()
            .map(|gap| (*gap.start(), *gap.end()))
    }

    /// The present ranges as `(start, end)` pairs, for a durable checkpoint.
    pub(crate) fn to_ranges(&self) -> Vec<(u64, u64)> {
        self.0.iter().map(|r| (*r.start(), *r.end())).collect()
    }
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_reports_the_frontier_advance() {
        let mut ranges = StoredRanges::default();
        assert_eq!(ranges.insert(1), Some(1)); // genesis lands → frontier 1
        assert_eq!(ranges.insert(2), Some(2)); // plain +1
        assert_eq!(ranges.insert(5), None); // island above the gap → no advance
        assert_eq!(ranges.insert(3), Some(3)); // +1
        assert_eq!(ranges.insert(4), Some(5)); // bridges to [5] → jumps to 5
    }

    #[test]
    fn contiguous_top_tracks_the_genesis_run() {
        let mut ranges = StoredRanges::default();
        assert_eq!(ranges.contiguous_top(), None); // empty

        ranges.insert(1);
        ranges.insert(2);
        ranges.insert(3);
        assert_eq!(ranges.contiguous_top(), Some(3));

        // A hole above the run does not extend the frontier.
        ranges.insert(5);
        assert_eq!(ranges.contiguous_top(), Some(3));
    }

    #[test]
    fn genesis_absent_means_no_frontier() {
        let mut ranges = StoredRanges::default();
        ranges.insert(2);
        ranges.insert(3); // 1 missing
        assert_eq!(ranges.contiguous_top(), None);
    }

    #[test]
    fn first_gap_is_the_lowest_hole() {
        let mut ranges = StoredRanges::default();
        ranges.insert(1);
        ranges.insert(2);
        ranges.insert(3);
        assert_eq!(ranges.first_gap(), None); // contiguous

        // {1..=3, 5..=6}: the lowest hole is [4, 4].
        ranges.insert(5);
        ranges.insert(6);
        assert_eq!(ranges.first_gap(), Some((4, 4)));

        // Fill 4 → {1..=6}: gap-free again.
        ranges.insert(4);
        assert_eq!(ranges.first_gap(), None);
    }

    #[test]
    fn first_gap_spans_below_the_lowest_block() {
        // The first stored block is 100 (a fresh store whose live tail landed
        // before the backfill): the gap is the whole history below it.
        let mut ranges = StoredRanges::default();
        ranges.insert(100);
        assert_eq!(ranges.first_gap(), Some((1, 99)));
    }

    #[test]
    fn nothing_is_a_gap_above_the_top() {
        let mut ranges = StoredRanges::default();
        ranges.insert(1);
        ranges.insert(2);
        // Contiguous to the top, so no hole — the open-ended future above 2 is
        // not reported as a gap.
        assert_eq!(ranges.first_gap(), None);
        assert_eq!(ranges.contiguous_top(), Some(2));
    }

    #[test]
    fn contains_and_checkpoint_roundtrip() {
        let mut ranges = StoredRanges::default();
        for height in [1, 2, 3, 7, 8] {
            ranges.insert(height);
        }
        assert!(ranges.contains(2));
        assert!(!ranges.contains(5));

        // Checkpoint → reload preserves the exact topology.
        let checkpoint = ranges.to_ranges();
        assert_eq!(checkpoint, vec![(1, 3), (7, 8)]);
        let reloaded = StoredRanges::from_ranges(&checkpoint);
        assert_eq!(reloaded.contiguous_top(), Some(3));
        assert_eq!(reloaded.first_gap(), Some((4, 6)));
    }
}
