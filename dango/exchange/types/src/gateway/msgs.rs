use {
    super::{Addr32, Origin, RateLimit, Remote},
    dango_math::Uint128,
    dango_primitives::{Addr, Coin, Denom, Duration, Op, Timestamp},
    std::collections::{BTreeMap, BTreeSet},
};

#[dango_primitives::derive(Serde)]
pub struct WithdrawalFee {
    pub denom: Denom,
    pub remote: Remote,

    /// Use `Op::Insert` to add a new fee or change an existing fee; use
    /// `Op::Delete` to remove a fee.
    pub fee: Op<Uint128>,
}

/// Admin input for `ExecuteMsg::SetPersonalQuota`. Carries the relative
/// lifetime `available_for`; the contract translates it into an absolute
/// `expiry` when saving the resulting `PersonalQuota`.
#[dango_primitives::derive(Serde)]
pub struct SetPersonalQuotaRequest {
    pub amount: Uint128,

    /// `None` means the quota never expires. `Some(d)` means the quota
    /// expires at `current_block_time + d`.
    pub available_for: Option<Duration>,
}

/// Per-account allowance that is consumed before the global outbound quota
/// when the user sends a remote transfer. This is the stored / returned
/// form; `SetPersonalQuotaRequest` is the admin input.
#[dango_primitives::derive(Borsh, Serde)]
pub struct PersonalQuota {
    pub amount: Uint128,

    /// `None` means the quota never expires. `Some(t)` means the quota is
    /// ignored once the current block timestamp reaches `t`.
    pub expire_at: Option<Timestamp>,

    /// The admin account that created or last overwrote this entry.
    pub granted_by: Addr,

    /// The block timestamp of the grant or most recent overwrite.
    pub granted_at: Timestamp,
}

/// A withdrawal held in escrow by the Gateway, awaiting a response from the
/// withdrawal guardian (or the chain owner).
///
/// The user's funds are transferred to the Gateway when the request is
/// created, and leave the Gateway only when the request reaches a terminal
/// response: approve (bridged out), reject (refunded to the user), or
/// confiscate (sent to the owner; frozen requests only).
#[dango_primitives::derive(Borsh, Serde)]
pub struct WithdrawalRequest {
    /// The Dango account that requested the withdrawal.
    pub user: Addr,
    /// The remote chain the tokens are to be sent to.
    pub remote: Remote,
    /// The recipient address on the remote chain.
    pub recipient: Addr32,
    /// The escrowed coin, in full. The withdrawal fee is deducted from it
    /// only if and when the request is approved.
    pub coin: Coin,
    pub status: WithdrawalStatus,
    pub created_at: Timestamp,
}

/// Pending and frozen requests are stored in separate maps, so the status
/// is implied by which queue holds the request; this field mirrors that
/// placement so a request remains self-describing when returned from the
/// single-request query. The freeze handler updates both together.
#[dango_primitives::derive(Borsh, Serde)]
#[derive(Copy, PartialOrd, Ord)]
pub enum WithdrawalStatus {
    /// Awaiting a response from the guardian or the owner.
    Pending,
    /// Flagged as suspicious. Only the owner can respond from here.
    Frozen,
}

/// A response to a pending or frozen withdrawal request.
#[dango_primitives::derive(Serde)]
#[derive(Copy, PartialOrd, Ord)]
pub enum WithdrawalResponse {
    /// Process the withdrawal: deduct the fee, enforce the rate limits, and
    /// bridge the tokens to the remote chain.
    Approve,
    /// Cancel the withdrawal and refund the full escrowed amount to the user.
    Reject,
    /// Flag the withdrawal as suspicious. A frozen request can only be
    /// resolved by the owner: approved, rejected, or confiscated.
    Freeze,
    /// Send the escrowed funds to the chain owner, in case of confirmed
    /// suspicious activity. Only the owner can do this, and only to a
    /// request that has been frozen first.
    Confiscate,
}

#[dango_primitives::derive(Serde)]
pub struct InstantiateMsg {
    pub routes: BTreeSet<(Origin, Addr, Remote)>,
    pub rate_limits: BTreeMap<Denom, RateLimit>,
    pub withdrawal_fees: Vec<WithdrawalFee>,
    /// The whitelisted address that responds to withdrawal requests. If
    /// unset, only the chain owner can respond.
    pub guardian: Option<Addr>,
}

#[dango_primitives::derive(Serde)]
pub enum ExecuteMsg {
    /// Create new routes.
    ///
    /// Can only be called by the chain owner.
    ///
    /// Note that this only creates or overwrites routes; to remove an
    /// existing route, use `RemoveRoutes`.
    SetRoutes(BTreeSet<(Origin, Addr, Remote)>),

    /// Remove existing routes, identified by `(bridge, remote)` tuples.
    ///
    /// Can only be called by the chain owner.
    ///
    /// Errors if:
    ///
    /// - any of the routes doesn't exist;
    /// - any of the routes still has a non-zero reserve. Removing such a
    ///   route would make it impossible for the reserve to be withdrawn,
    ///   as outbound transfers require the route to exist. Local-origin
    ///   routes never track a reserve, so they can always be removed.
    ///
    /// Withdrawal fees and rate limits are configured independently of
    /// routes, and are not affected by this.
    RemoveRoutes(BTreeSet<(Addr, Remote)>),

    /// Overwrite the global rate-limit configuration.
    ///
    /// The map is the new complete set of rate-limited denoms:
    ///
    /// - A denom absent from the map is not rate-limited — outbound
    ///   transfers of it are unrestricted (only reserves and withdrawal
    ///   fees still apply). Any existing supply snapshot and trailing-
    ///   window history for a dropped denom are cleared in the same block.
    /// - A denom mapped to `0` is fully locked — the cap is set to zero,
    ///   so no outbound transfer passes until the admin raises the limit
    ///   or removes the denom. Setting `0` also revokes every outstanding
    ///   personal quota for that denom, so a granted user cannot bypass
    ///   the freeze via their per-account allowance.
    /// - A denom mapped to a positive fraction less than `1` is enforced
    ///   as a trailing-24h cap: a withdraw is rejected when the sum of
    ///   withdraws over the trailing 24 hours plus the new request would
    ///   exceed `supply_snapshot × limit`. The supply snapshot is taken
    ///   by the cron handler once per refresh period and is also seeded
    ///   on the first `SetRateLimits` that adds the denom.
    ///
    /// A configured-limit change takes effect immediately on the next
    /// withdraw (`cap` rises or falls with `limit`), but the supply
    /// snapshot is not refreshed by this call — it only moves at cron
    /// ticks, so deposits between cron ticks cannot enlarge the cap.
    ///
    /// Can only be called by the chain owner.
    SetRateLimits(BTreeMap<Denom, RateLimit>),

    /// Set withdrawal fees for the denoms.
    SetWithdrawalFees(Vec<WithdrawalFee>),

    /// Grant or revoke a per-account, per-denom withdrawal allowance that is
    /// consumed before the global outbound quota.
    ///
    /// `Op::Insert(request)` overwrites any existing entry for the same
    /// `(user, denom)` with the fields in `request`. `Op::Delete` removes
    /// the entry entirely.
    ///
    /// Can only be called by the chain owner.
    SetPersonalQuota {
        user: Addr,
        denom: Denom,
        quota: Op<SetPersonalQuotaRequest>,
    },

    /// Set or overwrite the withdrawal guardian: the whitelisted address
    /// that responds to withdrawal requests.
    ///
    /// Can only be called by the chain owner.
    SetGuardian(Addr),

    /// Receive a token transfer from a remote chain.
    ///
    /// Can only be called by contracts for which has been assigned a
    ReceiveRemote {
        remote: Remote,
        amount: Uint128,
        recipient: Addr,
    },

    /// Request a token transfer to a remote chain.
    ///
    /// The attached funds are held in escrow by the Gateway, and a
    /// `WithdrawalRequest` is stored. The withdrawal is executed only once
    /// the guardian (or the owner) approves the request.
    ///
    /// Can be called by anyone.
    TransferRemote { remote: Remote, recipient: Addr32 },

    /// Respond to a withdrawal request:
    ///
    /// - `Approve`: process the withdrawal. This enforces the rate limits
    ///   at the time of the response, not the time of the request. If the
    ///   withdrawal can no longer be executed (the fee, reserve, or rate
    ///   limit changed while the request was pending), the escrow is
    ///   refunded to the user — emitting `withdrawal_approval_failed` —
    ///   so an approval always settles the request.
    /// - `Reject`: refund the escrowed funds to the user.
    /// - `Freeze`: flag the request as suspicious for the owner to check.
    /// - `Confiscate`: send the escrowed funds to the owner.
    ///
    /// Can be called by the withdrawal guardian or the chain owner, with
    /// two restrictions: a frozen request can only be responded to by the
    /// owner, and `Confiscate` is owner-only and requires the request to
    /// have been frozen first.
    RespondToWithdrawal {
        id: u64,
        response: WithdrawalResponse,
    },
}

#[dango_primitives::derive(Serde, QueryRequest)]
pub enum QueryMsg {
    /// Given a `(bridge, remote)` tuple, find the alloyed denom it belongs to.
    #[returns(Option<Denom>)]
    Route { bridge: Addr, remote: Remote },

    /// Given an alloyed denom and the remote, find the bridge contract that handles it.
    #[returns(Option<Addr>)]
    ReverseRoute { denom: Denom, remote: Remote },

    /// Enumerate all routes.
    #[returns(Vec<QueryRoutesResponseItem>)]
    Routes {
        start_after: Option<(Addr, Remote)>,
        limit: Option<u32>,
    },

    /// Given a `(bridge, remote)` tuple, find the reserve amount.
    #[returns(Uint128)]
    Reserve { bridge: Addr, remote: Remote },

    /// Enumerate all reserves.
    #[returns(Vec<QueryReservesResponseItem>)]
    Reserves {
        start_after: Option<(Addr, Remote)>,
        limit: Option<u32>,
    },

    /// Given a `(denom, remote)` tuple, find the withdrawal fee.
    #[returns(Uint128)]
    WithdrawalFee { denom: Denom, remote: Remote },

    /// Enumerate all withdrawal fees.
    #[returns(Vec<QueryWithdrawalFeesResponseItem>)]
    WithdrawalFees {
        start_after: Option<(Denom, Remote)>,
        limit: Option<u32>,
    },

    /// Look up the personal quota an account has for a given denom.
    #[returns(Option<PersonalQuota>)]
    PersonalQuota { user: Addr, denom: Denom },

    /// Enumerate all personal quotas.
    #[returns(Vec<QueryPersonalQuotasResponseItem>)]
    PersonalQuotas {
        start_after: Option<(Addr, Denom)>,
        limit: Option<u32>,
    },

    /// Query the withdrawal guardian. Returns `None` if no guardian is set,
    /// in which case only the chain owner can respond to withdrawal requests.
    #[returns(Option<Addr>)]
    Guardian {},

    /// Look up a withdrawal request by ID, whether pending or frozen. Only
    /// those two states are stored; a request that has reached a terminal
    /// response (approved, rejected, or confiscated) is deleted, so this
    /// returns `None` for it.
    #[returns(Option<WithdrawalRequest>)]
    WithdrawalRequest { id: u64 },

    /// Enumerate pending withdrawal requests — the guardian's work queue.
    /// Frozen requests live in a separate queue, enumerated by
    /// `FrozenWithdrawalRequests`, so the guardian doesn't re-read requests
    /// it has already flagged.
    #[returns(Vec<QueryWithdrawalRequestsResponseItem>)]
    WithdrawalRequests {
        start_after: Option<u64>,
        limit: Option<u32>,
    },

    /// Enumerate frozen withdrawal requests — the owner's work queue.
    #[returns(Vec<QueryWithdrawalRequestsResponseItem>)]
    FrozenWithdrawalRequests {
        start_after: Option<u64>,
        limit: Option<u32>,
    },

    /// Query the withdraw rate limits.
    #[returns(BTreeMap<Denom, RateLimit>)]
    RateLimits {},

    /// Look up the rate-limit status for a denom: the supply snapshot, the
    /// derived cap, and the trailing-24h withdraw volume. Returns `None` if
    /// the denom is not rate-limited.
    #[returns(Option<RateLimitStatus>)]
    RateLimitStatus { denom: Denom },

    /// Enumerate the rate-limit status for every rate-limited denom.
    #[returns(BTreeMap<Denom, RateLimitStatus>)]
    RateLimitStatuses {
        start_after: Option<Denom>,
        limit: Option<u32>,
    },
}

#[dango_primitives::derive(Serde)]
pub struct QueryRoutesResponseItem {
    pub bridge: Addr,
    pub remote: Remote,
    pub denom: Denom,
}

#[dango_primitives::derive(Serde)]
pub struct QueryReservesResponseItem {
    pub bridge: Addr,
    pub remote: Remote,
    pub reserve: Uint128,
}

#[dango_primitives::derive(Serde)]
pub struct QueryWithdrawalFeesResponseItem {
    pub denom: Denom,
    pub remote: Remote,
    pub fee: Uint128,
}

#[dango_primitives::derive(Serde)]
pub struct QueryWithdrawalRequestsResponseItem {
    pub id: u64,
    pub request: WithdrawalRequest,
}

#[dango_primitives::derive(Serde)]
pub struct QueryPersonalQuotasResponseItem {
    pub user: Addr,
    pub denom: Denom,
    pub quota: PersonalQuota,
}

/// Rate-limit status for a single rate-limited denom: the supply snapshot
/// taken at the last cron tick (or at the denom's first registration), the
/// derived cap `supply_snapshot × limit`, and the rolling sum of withdraws
/// over the trailing 24 hours. Available headroom is `cap − used_in_last_24h`.
#[dango_primitives::derive(Serde)]
pub struct RateLimitStatus {
    pub supply_snapshot: Uint128,
    pub cap: Uint128,
    pub used_in_last_24h: Uint128,
}
