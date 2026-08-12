use {
    crate::{
        auth::{AccountStatus, Nonce},
        gateway::{Addr32, Remote},
    },
    dango_primitives::{ByteArray, Denom},
    std::collections::BTreeSet,
};

#[dango_primitives::derive(Serde)]
pub struct InstantiateMsg {
    /// Whether this account is to be activated upon instantiation.
    /// If not, a minimum deposit is required to activate the account.
    pub activate: bool,
}

/// Execute messages for the single-signature account
#[dango_primitives::derive(Serde)]
pub enum ExecuteMsg {
    /// Send the account's entire balance of `denom` to a remote chain, through
    /// the gateway contract.
    ///
    /// Can only be called by the chain owner.
    ///
    /// Dango is being shut down. Balances left behind by users who don't
    /// withdraw before the shutdown are returned to the address they deposited
    /// from, which the owner supplies as `recipient`.
    ///
    /// This holds no privilege inside the gateway. Routes, reserves, fees,
    /// personal quotas, and rate limits apply exactly as they do to a
    /// withdrawal the user makes themselves, and the resulting withdrawal
    /// request still needs the guardian's or the owner's approval.
    ForceWithdrawal {
        denom: Denom,
        remote: Remote,
        recipient: Addr32,
    },
}

/// Query messages for the single-signature account
#[dango_primitives::derive(Serde, QueryRequest)]
pub enum QueryMsg {
    /// Query the account's status.
    #[returns(AccountStatus)]
    Status {},
    /// Query the most recent transaction nonces recorded for standard
    /// (master-key) credentials.
    #[returns(BTreeSet<Nonce>)]
    SeenNonces {},
    /// Query the most recent transaction nonces recorded for the given session
    /// key.
    #[returns(BTreeSet<Nonce>)]
    SessionSeenNonces { session_key: ByteArray<33> },
}
