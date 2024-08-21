use {
    crate::{Account, Addr, Binary, BlockInfo, Coin, Coins, Config, Hash256, Json},
    serde::{Deserialize, Serialize},
    serde_with::skip_serializing_none,
    std::collections::BTreeMap,
};

/// Represents a query request to a contract.
///
/// A contract typically exposes multiple query methods, with a `QueryMsg` as an
/// enum with multiple variants. A `QueryRequest` represents one such variant.
pub trait QueryRequest: Sized {
    /// The full query message enum that contains this request.
    type Message: From<Self>;

    /// The response type for this query.
    type Response;
}

pub trait AlternativeQuery {
    type Alternative;

    fn into_alternative(self) -> Self::Alternative;
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    /// The chain's global information. Corresponding to the ABCI Info method.
    /// Returns: `InfoResponse`
    Info {},
    /// A single application-specific configuration.
    /// Returns: `Json`
    AppConfig { key: String },
    /// Enumerate all application-specific configurations.
    /// Returns: `BTreeMap<String, Json>`
    AppConfigs {
        start_after: Option<String>,
        limit: Option<u32>,
    },
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
    /// Returns: `BTreeMap<Hash256, Binary>`
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
    /// Perform multiple queries at once.
    /// Returns: `Vec<QueryResponse>`.
    Multi(Vec<Query>),
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
    AppConfig(Json),
    AppConfigs(BTreeMap<String, Json>),
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
    Multi(Vec<QueryResponse>),
}

// TODO: can we use a macro to implement these?
impl QueryResponse {
    pub fn as_info(self) -> InfoResponse {
        let Self::Info(resp) = self else {
            panic!("QueryResponse is not Info");
        };
        resp
    }

    pub fn as_app_config(self) -> Json {
        let Self::AppConfig(value) = self else {
            panic!("QueryResponse is not AppCofnig");
        };
        value
    }

    pub fn as_app_configs(self) -> BTreeMap<String, Json> {
        let Self::AppConfigs(map) = self else {
            panic!("QueryResponse is not AppCofnigs");
        };
        map
    }

    pub fn as_balance(self) -> Coin {
        let Self::Balance(coin) = self else {
            panic!("BankQueryResponse is not Balance");
        };
        coin
    }

    pub fn as_balances(self) -> Coins {
        let Self::Balances(coins) = self else {
            panic!("BankQueryResponse is not Balances");
        };
        coins
    }

    pub fn as_supply(self) -> Coin {
        let Self::Supply(coin) = self else {
            panic!("BankQueryResponse is not Supply");
        };
        coin
    }

    pub fn as_supplies(self) -> Coins {
        let Self::Supplies(coins) = self else {
            panic!("BankQueryResponse is not Supplies");
        };
        coins
    }

    pub fn as_code(self) -> Binary {
        let Self::Code(wasm_byte_code) = self else {
            panic!("QueryResponse is not Code");
        };
        wasm_byte_code
    }

    pub fn as_codes(self) -> BTreeMap<Hash256, Binary> {
        let Self::Codes(hashes) = self else {
            panic!("QueryResponse is not Codes");
        };
        hashes
    }

    pub fn as_account(self) -> Account {
        let Self::Account(resp) = self else {
            panic!("QueryResponse is not Account");
        };
        resp
    }

    pub fn as_accounts(self) -> BTreeMap<Addr, Account> {
        let Self::Accounts(resp) = self else {
            panic!("QueryResponse is not Accounts");
        };
        resp
    }

    pub fn as_wasm_raw(self) -> Option<Binary> {
        let Self::WasmRaw(resp) = self else {
            panic!("QueryResponse is not WasmRaw");
        };
        resp
    }

    pub fn as_wasm_smart(self) -> Json {
        let Self::WasmSmart(resp) = self else {
            panic!("QueryResponse is not WasmSmart");
        };
        resp
    }

    pub fn as_multi(self) -> Vec<QueryResponse> {
        let Self::Multi(resp) = self else {
            panic!("QueryResponse is not Multi");
        };
        resp
    }
}
