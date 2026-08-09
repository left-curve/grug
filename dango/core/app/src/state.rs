use {
    dango_primitives::{
        Addr, BlockInfo, Code, CodeStatus, Config, ContractInfo, Hash256, Json, NextUpgrade,
        PastUpgrade, Timestamp,
    },
    dango_storage::{Index, IndexList, IndexedMap, Item, Map, MultiIndex, Set},
};

/// All storage namespaces must have the same length. As such, when iterating
/// within the same namespace, we can skip the namespace. This makes the creation
/// of RocksDB iterators significantly faster.
pub const NAMESPACE_LEN: usize = 4;

/// Check at compile time that all namespaces are of the correct length.
const fn namespace(s: &str) -> &str {
    assert!(
        s.len() == NAMESPACE_LEN,
        "storage namespaces must be exactly 4 bytes long"
    );

    s
}

/// A string that identifies the chain
pub const CHAIN_ID: Item<String> = Item::new(namespace("chid"));

/// The most recently finalized block
pub const LAST_FINALIZED_BLOCK: Item<BlockInfo> = Item::new(namespace("lfbk"));

/// Chain-level configuration
pub const CONFIG: Item<Config> = Item::new(namespace("cnfg"));

/// Application-specific configuration.
pub const APP_CONFIG: Item<Json> = Item::new(namespace("acfg"));

/// Scheduled cronjobs.
///
/// This needs to be a `Set` instead of `Map<Timestamp, Addr>` because there can
/// be multiple jobs with the same scheduled time.
pub const NEXT_CRONJOBS: Set<(Timestamp, Addr)> = Set::new(namespace("crjb"));

/// A chain upgrade that is scheduled to happen in a future block.
pub const NEXT_UPGRADE: Item<NextUpgrade> = Item::new(namespace("nxup"));

/// Chain upgrades that have been carried out in the past.
pub const PAST_UPGRADES: Map<u64, PastUpgrade> = Map::new(namespace("pvup"));

/// Wasm contract byte codes: code_hash => byte_code
pub const CODES: IndexedMap<Hash256, Code, CodeIndexes> = IndexedMap::new(
    namespace("code"),
    CodeIndexes {
        status: MultiIndex::new(|_, c| c.status, namespace("code"), namespace("cdst")),
    },
);

/// Contract metadata: address => contract_info
pub const CONTRACTS: Map<Addr, ContractInfo> = Map::new(namespace("ctrt"));

/// Each contract has its own storage space, which we term the "substore".
/// A key in a contract's substore is prefixed by the word "wasm" + contract address.
pub const CONTRACT_NAMESPACE: &[u8] = b"wasm";

pub struct CodeIndexes<'a> {
    pub status: MultiIndex<'a, Hash256, CodeStatus, Code>,
}

impl IndexList<Hash256, Code> for CodeIndexes<'_> {
    fn get_indexes(&self) -> Box<dyn Iterator<Item = &'_ dyn Index<Hash256, Code>> + '_> {
        let v: Vec<&dyn Index<Hash256, Code>> = vec![&self.status];
        Box::new(v.into_iter())
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::CODES,
        dango_primitives::{
            Binary, Code, CodeStatus, Duration, Hash256, MockStorage, Order, StdResult, Timestamp,
        },
        dango_storage::PrefixBound,
    };

    #[test]
    fn codes() {
        let mut storage = MockStorage::new();

        CODES
            .save(
                &mut storage,
                Hash256::from_inner([0; 32]),
                &Code {
                    code: Binary::from_inner(vec![0; 32]),
                    status: CodeStatus::Orphaned {
                        since: Duration::from_seconds(100),
                    },
                },
            )
            .unwrap();

        CODES
            .save(
                &mut storage,
                Hash256::from_inner([1; 32]),
                &Code {
                    code: Binary::from_inner(vec![1; 32]),
                    status: CodeStatus::Orphaned {
                        since: Duration::from_seconds(20),
                    },
                },
            )
            .unwrap();

        CODES
            .save(
                &mut storage,
                Hash256::from_inner([2; 32]),
                &Code {
                    code: Binary::from_inner(vec![2; 32]),
                    status: CodeStatus::InUse { usage: 12345 },
                },
            )
            .unwrap();

        CODES
            .save(
                &mut storage,
                Hash256::from_inner([3; 32]),
                &Code {
                    code: Binary::from_inner(vec![3; 32]),
                    status: CodeStatus::InUse { usage: 88888 },
                },
            )
            .unwrap();

        // Find _all_ codes, regardless of orphaned or in use.
        {
            let res = CODES
                .idx
                .status
                .prefix_keys(&storage, None, None, Order::Ascending)
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                res,
                [
                    // Orphaned nodes are ordered by orphan time.
                    (
                        CodeStatus::Orphaned {
                            since: Timestamp::from_seconds(20),
                        },
                        Hash256::from_inner([1; 32]),
                    ),
                    (
                        CodeStatus::Orphaned {
                            since: Timestamp::from_seconds(100),
                        },
                        Hash256::from_inner([0; 32]),
                    ),
                    // In-use nodes are ordered by usage count.
                    (
                        CodeStatus::InUse { usage: 12345 },
                        Hash256::from_inner([2; 32]),
                    ),
                    (
                        CodeStatus::InUse { usage: 88888 },
                        Hash256::from_inner([3; 32]),
                    )
                ]
            );
        }

        // Find all orphaned codes whose orphan time is earlier or equal to 100.
        {
            let res = CODES
                .idx
                .status
                .prefix_keys(
                    &storage,
                    None,
                    Some(PrefixBound::Inclusive(CodeStatus::Orphaned {
                        since: Duration::from_seconds(100),
                    })),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                res,
                [
                    (
                        CodeStatus::Orphaned {
                            since: Timestamp::from_seconds(20),
                        },
                        Hash256::from_inner([1; 32]),
                    ),
                    (
                        CodeStatus::Orphaned {
                            since: Timestamp::from_seconds(100),
                        },
                        Hash256::from_inner([0; 32]),
                    ),
                ]
            );
        }

        // Find all orphaned codes whose orphan time is earlier or equal to 30.
        {
            let res = CODES
                .idx
                .status
                .prefix_keys(
                    &storage,
                    None,
                    Some(PrefixBound::Inclusive(CodeStatus::Orphaned {
                        since: Duration::from_seconds(30),
                    })),
                    Order::Ascending,
                )
                .collect::<StdResult<Vec<_>>>()
                .unwrap();

            assert_eq!(
                res,
                [(
                    CodeStatus::Orphaned {
                        since: Timestamp::from_seconds(20),
                    },
                    Hash256::from_inner([1; 32]),
                )]
            );
        }
    }
}
