use {
    crate::bank::Metadata,
    dango_primitives::{Addr, Coins, Denom, Part},
    std::collections::BTreeMap,
};

#[dango_primitives::derive(Serde)]
pub struct InstantiateMsg {
    /// Initial account balances.
    pub balances: BTreeMap<Addr, Coins>,
    /// Initial namespace ownerships.
    pub namespaces: BTreeMap<Part, Addr>,
    /// Initial denom metadatas.
    pub metadatas: BTreeMap<Denom, Metadata>,
}

#[dango_primitives::derive(Serde)]
pub enum ExecuteMsg {
    /// Grant the control of a namespace to an account.
    /// Can only be called by the chain owner.
    ///
    /// Currently, we don't support:
    ///
    /// 1. granting the top-level namespace;
    /// 2. chain owner canceling a grant;
    /// 3. namespace owner renouncing or transferring a grant;
    /// 4. a namespace to have more than one owner;
    /// 5. before-send hooks.
    ///
    /// We may implement some of these in the future.
    SetNamespaceOwner { namespace: Part, owner: Addr },
    /// Set metadata of a denom.
    /// Can only be called by the namespace owner, or the chain owner in case of
    /// top-level denoms.
    SetMetadata { denom: Denom, metadata: Metadata },
    /// Mint tokens of the specified amount to a recipient.
    /// Can only be called by the namespace owner.
    Mint { to: Addr, coins: Coins },
    /// Burn tokens of the specified amount from an account.
    /// Can only be called by the namespace owner.
    Burn { from: Addr, coins: Coins },
    /// Retrieve funds sent to a non-existing recipient.
    /// Can only be called by the transfer's sender, recipient, or the chain owner.
    RecoverTransfer { sender: Addr, recipient: Addr },
    /// Enable or disable token transfers.
    /// Can only be called by the chain owner.
    ///
    /// While disabled, the only transfers that go through are those to or from
    /// the gateway contract, and those to the chain owner. See the comments on
    /// `bank_execute` for why.
    SetTransfersEnabled(bool),
}

#[dango_primitives::derive(Serde, QueryRequest)]
pub enum QueryMsg {
    /// Query the owner of a namespace.
    #[returns(Addr)]
    NamespaceOwner { namespace: Part },
    /// Enumerate owners of all namespaces.
    #[returns(BTreeMap<Part, Addr>)]
    NamespaceOwners {
        start_after: Option<Part>,
        limit: Option<u32>,
    },
    /// Query the metadata of a denom.
    #[returns(Metadata)]
    Metadata { denom: Denom },
    /// Enumerate metadata of all denoms.
    #[returns(BTreeMap<Denom, Metadata>)]
    Metadatas {
        start_after: Option<Denom>,
        limit: Option<u32>,
    },
    /// Query the orphaned transfer amount between a sender and a recipient.
    #[returns(Coins)]
    OrphanedTransfer { sender: Addr, recipient: Addr },
    /// Enumerate orphaned transfers among all senders and recipients.
    #[returns(Vec<OrphanedTransferResponseItem>)]
    OrphanedTransfers {
        start_after: Option<OrphanedTransferPageParam>,
        limit: Option<u32>,
    },
    /// Enumerate orphaned transfer originated from a sender.
    #[returns(BTreeMap<Addr, Coins>)]
    OrphanedTransfersBySender {
        sender: Addr,
        start_after: Option<Addr>,
        limit: Option<u32>,
    },
    /// Enumerate orphaned transfer destined to a recipient.
    #[returns(BTreeMap<Addr, Coins>)]
    OrphanedTransfersByRecipient {
        recipient: Addr,
        start_after: Option<Addr>,
        limit: Option<u32>,
    },
    /// Query whether token transfers are enabled.
    #[returns(bool)]
    TransfersEnabled {},
}

#[dango_primitives::derive(Serde)]
pub struct OrphanedTransferPageParam {
    pub sender: Addr,
    pub recipient: Addr,
}

#[dango_primitives::derive(Serde)]
pub struct OrphanedTransferResponseItem {
    pub sender: Addr,
    pub recipient: Addr,
    pub amount: Coins,
}
