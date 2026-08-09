use {
    dango_math::Uint128,
    dango_primitives::{Addr, Coins, Denom, Part},
    dango_storage::{IndexedMap, Map, MultiIndex},
    dango_types::bank::Metadata,
};

pub const NAMESPACE_OWNERS: Map<&Part, Addr> = Map::new("namespace_owner");

pub const METADATAS: Map<&Denom, Metadata> = Map::new("metadata");

pub const SUPPLIES: Map<&Denom, Uint128> = Map::new("supply");

pub const BALANCES: Map<(&Addr, &Denom), Uint128> = Map::new("balance");

// (sender, recipient) -> coins
pub const ORPHANED_TRANSFERS: IndexedMap<(Addr, Addr), Coins, OrphanedTransferIndexes> =
    IndexedMap::new(
        "orphaned_transfer",
        OrphanedTransferIndexes {
            recipient: MultiIndex::new(
                |(_, recipient), _| *recipient,
                "orphaned_transfer",
                "orphaned_transfer__recipient",
            ),
        },
    );

#[dango_storage::index_list((Addr, Addr), Coins)]
pub struct OrphanedTransferIndexes<'a> {
    pub recipient: MultiIndex<'a, (Addr, Addr), Addr, Coins>,
}
