use {
    crate::{Addr, Coin, Coins},
    borsh::{BorshDeserialize, BorshSerialize},
    serde::{Deserialize, Serialize},
    serde_with::skip_serializing_none,
};

/// The execute message that the host provides the bank contract during the
/// `bank_execute` function call.
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct BankMsg {
    pub from: Addr,
    pub to: Addr,
    pub coins: Coins,
}

/// The query message that the host provides the bank contract during the
/// `bank_query` function call.
#[skip_serializing_none]
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BankQuery {
    Balance {
        address: Addr,
        denom: String,
    },
    Balances {
        address: Addr,
        start_after: Option<String>,
        limit: Option<u32>,
    },
    Supply {
        denom: String,
    },
    Supplies {
        start_after: Option<String>,
        limit: Option<u32>,
    },
}

/// The query response that the bank contract must return during the `bank_query`
/// function call.
///
/// The response MUST match the query. For example, if the host queries
/// `BankQuery::Balance`, the contract must return `BankQueryResponse::Balance`.
/// Returning a different `BankQueryResponse` variant can cause the host to
/// panic and the chain halted.
///
/// This said, we don't consider this a security vulnerability, because bank is
/// a _privileged contract_ that must be approved by governance.
#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BankQueryResponse {
    Balance(Coin),
    Balances(Coins),
    Supply(Coin),
    Supplies(Coins),
}

impl BankQueryResponse {
    pub fn as_balance(self) -> Coin {
        let BankQueryResponse::Balance(coin) = self else {
            panic!("BankQueryResponse is not Balance");
        };
        coin
    }

    pub fn as_balances(self) -> Coins {
        let BankQueryResponse::Balances(coins) = self else {
            panic!("BankQueryResponse is not Balances");
        };
        coins
    }

    pub fn as_supply(self) -> Coin {
        let BankQueryResponse::Supply(coin) = self else {
            panic!("BankQueryResponse is not Supply");
        };
        coin
    }

    pub fn as_supplies(self) -> Coins {
        let BankQueryResponse::Supplies(coins) = self else {
            panic!("BankQueryResponse is not Supplies");
        };
        coins
    }
}
