use {
    super::{Addr32, Remote},
    dango_math::Uint128,
    dango_primitives::{Addr, Denom},
};

/// Event indicating tokens have been received from a remote chain and
/// credited to a Dango account (a deposit).
#[dango_primitives::derive(Serde)]
#[dango_primitives::event("deposited")]
pub struct Deposited {
    /// The Dango account that received the tokens.
    pub user: Addr,
    /// The bridge contract that relayed the transfer.
    pub bridge: Addr,
    /// The remote chain the tokens originated from.
    pub remote: Remote,
    pub denom: Denom,
    pub amount: Uint128,
}

/// Event indicating a user has requested a withdrawal to a remote chain.
/// The funds are held in escrow by the Gateway until the withdrawal
/// guardian (or the chain owner) responds to the request.
#[dango_primitives::derive(Serde)]
#[dango_primitives::event("withdrawal_requested")]
pub struct WithdrawalRequested {
    pub id: u64,
    /// The Dango account that requested the withdrawal.
    pub user: Addr,
    /// The remote chain the tokens are to be sent to.
    pub remote: Remote,
    /// The recipient address on the remote chain.
    pub recipient: Addr32,
    pub denom: Denom,
    /// The full escrowed amount, before any withdrawal fee.
    pub amount: Uint128,
}

/// Event indicating a withdrawal request has been rejected, and the escrowed
/// funds refunded to the user.
#[dango_primitives::derive(Serde)]
#[dango_primitives::event("withdrawal_rejected")]
pub struct WithdrawalRejected {
    pub id: u64,
    /// The Dango account that requested the withdrawal and got the refund.
    pub user: Addr,
    pub denom: Denom,
    /// The full escrowed amount refunded to the user.
    pub amount: Uint128,
    /// The guardian or owner account that rejected the request.
    pub rejected_by: Addr,
}

/// Event indicating a withdrawal request has been frozen. Only the chain
/// owner can respond to it from here.
#[dango_primitives::derive(Serde)]
#[dango_primitives::event("withdrawal_frozen")]
pub struct WithdrawalFrozen {
    pub id: u64,
    /// The Dango account that requested the withdrawal.
    pub user: Addr,
    pub denom: Denom,
    /// The full escrowed amount.
    pub amount: Uint128,
    /// The guardian or owner account that froze the request.
    pub frozen_by: Addr,
}

/// Event indicating a frozen withdrawal request has been confiscated: the
/// escrowed funds were sent to the chain owner due to suspicious activity.
#[dango_primitives::derive(Serde)]
#[dango_primitives::event("withdrawal_confiscated")]
pub struct WithdrawalConfiscated {
    pub id: u64,
    /// The Dango account that requested the withdrawal.
    pub user: Addr,
    pub denom: Denom,
    /// The full escrowed amount sent to the owner.
    pub amount: Uint128,
}

/// Event indicating an approved withdrawal request could not be executed
/// against the current state — the fee, reserve, or rate limit changed
/// while the request was pending — and the escrowed funds were refunded
/// to the user instead.
#[dango_primitives::derive(Serde)]
#[dango_primitives::event("withdrawal_approval_failed")]
pub struct WithdrawalApprovalFailed {
    pub id: u64,
    /// The Dango account that requested the withdrawal and got the refund.
    pub user: Addr,
    pub denom: Denom,
    /// The full escrowed amount refunded to the user.
    pub amount: Uint128,
    /// Why the withdrawal could not be executed.
    pub reason: String,
}

/// Event indicating tokens have been sent to a remote chain (a withdrawal).
/// Emitted when a withdrawal request is approved.
#[dango_primitives::derive(Serde)]
#[dango_primitives::event("withdrawn")]
pub struct Withdrawn {
    /// The ID of the approved withdrawal request.
    pub id: u64,
    /// The Dango account that initiated the withdrawal.
    pub user: Addr,
    /// The bridge contract that handles the transfer.
    pub bridge: Addr,
    /// The remote chain the tokens are sent to.
    pub remote: Remote,
    /// The recipient address on the remote chain.
    pub recipient: Addr32,
    pub denom: Denom,
    /// The amount bridged to the remote chain, after deducting the
    /// withdrawal fee.
    pub amount: Uint128,
    /// The withdrawal fee charged; zero if no fee is configured for the
    /// route.
    pub fee: Uint128,
}
