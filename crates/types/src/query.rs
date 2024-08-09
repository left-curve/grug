use {
    crate::{Account, Addr, Binary, BlockInfo, Coin, Coins, Config, Hash256, Json},
    serde::{Deserialize, Serialize},
    serde_with::skip_serializing_none,
    std::collections::BTreeMap,
};

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryRequest {
    /// The chain's global information. Corresponding to the ABCI Info method.
    /// Returns: `InfoResponse`
    Info {},
    /// An account's balance in a single denom.
    /// Returns: `Coin`
    Balance { address: Addr, denom: String },
    /// Enumerate an account's balances in all denoms.
    /// Returns: `Coins`
    Balances {
        address: Addr,
        start_after: Option<String>,
        limit: Option<u32>,
    },
    /// A token's total supply.
    /// Returns: `Coin`
    Supply { denom: String },
    /// Enumerate all token's total supplies.
    /// Returns: `Coins`
    Supplies {
        start_after: Option<String>,
        limit: Option<u32>,
    },
    /// A single Wasm byte code.
    /// Returns: `Binary`
    Code { hash: Hash256 },
    /// Enumerate all Wasm byte codes.
    ///
    /// Returns: `BTreeMap<Hash, Binary>`
    Codes {
        start_after: Option<Hash256>,
        limit: Option<u32>,
    },
    /// Metadata of a single account.
    /// Returns: `Account`
    Account { address: Addr },
    /// Enumerate metadata of all accounts.
    /// Returns: `BTreeMap<Addr, Account>`
    Accounts {
        start_after: Option<Addr>,
        limit: Option<u32>,
    },
    /// A raw key-value pair in a contract's internal state.
    /// Returns: `Option<Binary>`
    WasmRaw { contract: Addr, key: Binary },
    /// Call the contract's query entry point with the given message.
    /// Returns: `Json`
    WasmSmart { contract: Addr, msg: Json },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InfoResponse {
    pub chain_id: String,
    pub config: Config,
    pub last_finalized_block: BlockInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryResponse {
    Info(InfoResponse),
    Balance(Coin),
    Balances(Coins),
    Supply(Coin),
    Supplies(Coins),
    Code(Binary),
    Codes(BTreeMap<Hash256, Binary>),
    Account(Account),
    Accounts(BTreeMap<Addr, Account>),
    WasmRaw(Option<Binary>),
    WasmSmart(Json),
}

macro_rules! generate_as {
    ($id:ident => $ret:ty) => {
        paste::paste! {
            pub fn [<as_$id:snake>](self) -> $ret {
                let Self::$id(resp) = self else {
                     panic!("QueryResponse is not {}", stringify!($id));
                };
                resp
            }
        }
    };
}

impl QueryResponse {
    generate_as!(Info => InfoResponse);

    generate_as!(Balance => Coin);

    generate_as!(Balances => Coins);

    generate_as!(Supply => Coin);

    generate_as!(Supplies => Coins);

    generate_as!(Code => Binary);

    generate_as!(Codes => BTreeMap<Hash256, Binary>);

    generate_as!(Account => Account);

    generate_as!(Accounts => BTreeMap<Addr, Account>);

    generate_as!(WasmRaw => Option<Binary>);

    generate_as!(WasmSmart => Json);
}
