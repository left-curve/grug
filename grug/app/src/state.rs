use {
    grug_storage::{Item, Map, Set},
    grug_types::{Addr, Binary, BlockInfo, Config, ContractInfo, Hash256, Json, Timestamp},
};

/// A string that identifies the chain
pub const CHAIN_ID: Item<String> = Item::new("chain_id");

/// Chain-level configuration
pub const CONFIG: Item<Config> = Item::new("config");

/// Application-specific configurations.
pub const APP_CONFIGS: Map<&str, Json> = Map::new("app_config");

/// The most recently finalized block
pub const LAST_FINALIZED_BLOCK: Item<BlockInfo> = Item::new("last_finalized_block");

/// Scheduled cronjobs.
///
/// This needs to be a `Set` instead of `Map<Timestamp, Addr>` because there can
/// be multiple jobs with the same scheduled time.
pub const NEXT_CRONJOBS: Set<(Timestamp, Addr)> = Set::new("jobs");

/// Wasm contract byte codes: code_hash => byte_code
pub const CODES: Map<Hash256, Binary> = Map::new("code");

/// Contract metadata: address => contract_info
pub const CONTRACTS: Map<Addr, ContractInfo> = Map::new("contract");

/// Each contract has its own storage space, which we term the "substore".
/// A key in a contract's substore is prefixed by the word "wasm" + contract address.
pub const CONTRACT_NAMESPACE: &[u8] = b"wasm";
