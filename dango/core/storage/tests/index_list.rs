#![cfg(feature = "macros")]

use {
    dango_primitives::{MockStorage, Order, StdResult},
    dango_storage::{IndexedMap, MultiIndex, UniqueIndex},
};

// To test the most general case, we create an index map where both the key and
// value are tuples.
#[dango_storage::index_list((u8, u64), (i8, i64))]
struct TestIndexes<'a> {
    pub foo: MultiIndex<'a, (u8, u64), u8, (i8, i64)>,
    pub bar: UniqueIndex<'a, (u8, u64), i64, (i8, i64)>,
}

const TEST: IndexedMap<(u8, u64), (i8, i64), TestIndexes> = IndexedMap::new(
    "test",
    TestIndexes {
        foo: MultiIndex::new(|k, _v| k.0, "test", "test__foo"),
        bar: UniqueIndex::new(|_k, v| v.1, "test", "test__bar"),
    },
);

#[test]
fn index_list_macro_works() {
    let mut storage = MockStorage::new();

    TEST.save(&mut storage, (1, 2), &(3, 4)).unwrap();

    let foos = TEST
        .idx
        .foo
        .range(&storage, None, None, Order::Ascending)
        .collect::<StdResult<Vec<_>>>()
        .unwrap();
    assert_eq!(foos, vec![(1, (1, 2), (3, 4))]);

    let bars = TEST
        .idx
        .bar
        .range(&storage, None, None, Order::Ascending)
        .collect::<StdResult<Vec<_>>>()
        .unwrap();
    assert_eq!(bars, vec![(4, (1, 2), (3, 4))]);
}
