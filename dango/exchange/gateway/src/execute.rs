use {
    crate::{
        FROZEN_WITHDRAWAL_REQUESTS, NEXT_WITHDRAWAL_REQUEST_ID, PERSONAL_QUOTAS, RESERVES,
        REVERSE_ROUTES, ROUTES, WITHDRAWAL_FEES, WITHDRAWAL_GUARDIAN, WITHDRAWAL_REQUESTS,
        rate_limit,
    },
    anyhow::{anyhow, bail, ensure},
    dango_math::{IsZero, Number, NumberConst, Uint128},
    dango_primitives::{
        Addr, Coin, Coins, Denom, Inner, Message, MutableCtx, Op, Order, QuerierExt, Response,
        StdError, StdResult, Storage, SudoCtx, Timestamp, coins,
    },
    dango_types::{
        bank,
        gateway::{
            Addr32, Deposited, ExecuteMsg, InstantiateMsg, NAMESPACE, Origin, PersonalQuota,
            RateLimit, Remote, SetPersonalQuotaRequest, Traceable, WithdrawalApprovalFailed,
            WithdrawalConfiscated, WithdrawalFee, WithdrawalFrozen, WithdrawalRejected,
            WithdrawalRequest, WithdrawalRequested, WithdrawalResponse, WithdrawalStatus,
            Withdrawn,
            bridge::{self, BridgeMsg},
        },
    },
    std::collections::{BTreeMap, BTreeSet},
};

pub fn instantiate(ctx: MutableCtx, msg: InstantiateMsg) -> anyhow::Result<Response> {
    _set_routes(ctx.storage, msg.routes)?;
    rate_limit::init(ctx.storage, msg.rate_limits)?;
    _set_withdrawal_fees(ctx.storage, msg.withdrawal_fees)?;

    if let Some(guardian) = msg.guardian {
        WITHDRAWAL_GUARDIAN.save(ctx.storage, &guardian)?;
    }

    Ok(Response::new())
}

pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> anyhow::Result<Response> {
    match msg {
        ExecuteMsg::SetRoutes(mapping) => set_routes(ctx, mapping),
        ExecuteMsg::RemoveRoutes(routes) => remove_routes(ctx, routes),
        ExecuteMsg::SetRateLimits(rate_limits) => set_rate_limits(ctx, rate_limits),
        ExecuteMsg::SetWithdrawalFees(withdrawal_fees) => set_withdrawal_fees(ctx, withdrawal_fees),
        ExecuteMsg::SetGuardian(guardian) => set_withdrawal_guardian(ctx, guardian),
        ExecuteMsg::ReceiveRemote {
            remote,
            amount,
            recipient,
        } => receive_remote(ctx, remote, amount, recipient),
        ExecuteMsg::TransferRemote { remote, recipient } => transfer_remote(ctx, remote, recipient),
        ExecuteMsg::RespondToWithdrawal { id, response } => {
            respond_to_withdrawal(ctx, id, response)
        },
        ExecuteMsg::SetPersonalQuota { user, denom, quota } => {
            set_personal_quota(ctx, user, denom, quota)
        },
    }
}

fn set_routes(
    ctx: MutableCtx,
    routes: BTreeSet<(Origin, Addr, Remote)>,
) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "only the owner can set routes"
    );

    _set_routes(ctx.storage, routes)?;

    Ok(Response::new())
}

fn _set_routes(
    storage: &mut dyn Storage,
    routes: BTreeSet<(Origin, Addr, Remote)>,
) -> anyhow::Result<()> {
    for (origin, bridge, remote) in routes {
        let denom = match origin {
            Origin::Local(denom) => {
                ensure!(
                    !denom.is_remote(),
                    "local denom must not start with `{}` namespace: `{}`",
                    NAMESPACE.as_ref(),
                    denom
                );

                denom
            },
            Origin::Remote(part) => Denom::from_parts([NAMESPACE.clone(), part])?,
        };

        ROUTES.save(storage, (bridge, remote), &denom)?;
        REVERSE_ROUTES.save(storage, (&denom, remote), &bridge)?;
    }

    Ok(())
}

fn remove_routes(ctx: MutableCtx, routes: BTreeSet<(Addr, Remote)>) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "only the owner can remove routes"
    );

    for (bridge, remote) in routes {
        // Load the denom of the route. Errors if the route doesn't exist.
        let denom = ROUTES.load(ctx.storage, (bridge, remote))?;

        // The reserve of the route must be zero: either no entry exists (the
        // route was never funded, or its denom is local-origin, for which
        // reserves aren't tracked), or the entry has been drained to exactly
        // zero. Otherwise, removing the route would make it impossible for
        // the reserve to be withdrawn, as `transfer_remote` requires the
        // reverse route to exist.
        let reserve = RESERVES
            .may_load(ctx.storage, (bridge, remote))?
            .unwrap_or(Uint128::ZERO);

        ensure!(
            reserve.is_zero(),
            "can't remove route with non-zero reserve! bridge: {bridge}, remote: {remote:?}, reserve: {reserve}"
        );

        // Delete the route, its reverse mapping, and the (zero-valued)
        // reserve entry, so that reserve enumeration doesn't show dangling
        // zeros. If the route is re-added later, `receive_remote` recreates
        // the reserve entry upon the first inbound transfer.
        ROUTES.remove(ctx.storage, (bridge, remote));
        REVERSE_ROUTES.remove(ctx.storage, (&denom, remote));
        RESERVES.remove(ctx.storage, (bridge, remote));
    }

    Ok(Response::new())
}

fn set_rate_limits(
    ctx: MutableCtx,
    rate_limits: BTreeMap<Denom, RateLimit>,
) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "only the owner can set rate limits"
    );

    // A 0% rate limit is a hard freeze: the global cap is zero, so it must
    // also revoke any personal quota that would otherwise let a user bypass
    // the freeze through their per-account allowance.
    //
    // Compute the frozen-denom set from the incoming map and run the
    // revocation pass before delegating to `rate_limit::apply_admin_update`.
    // Personal quotas are not rate-limit machinery and live outside the
    // `rate_limit` module; keeping the revocation here means that module
    // doesn't have to know about `PERSONAL_QUOTAS`.
    let frozen_denoms: BTreeSet<&Denom> = rate_limits
        .iter()
        .filter(|(_, limit)| limit.into_inner().is_zero())
        .map(|(denom, _)| denom)
        .collect();

    if !frozen_denoms.is_empty() {
        let personal_quotas = PERSONAL_QUOTAS
            .range(ctx.storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()?;

        for ((user, denom), _) in personal_quotas {
            if frozen_denoms.contains(&denom) {
                PERSONAL_QUOTAS.remove(ctx.storage, (user, &denom));
            }
        }
    }

    rate_limit::apply_admin_update(ctx.storage, ctx.querier, rate_limits)?;

    Ok(Response::new())
}

fn set_withdrawal_fees(
    ctx: MutableCtx,
    withdrawal_fees: Vec<WithdrawalFee>,
) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "only the owner can set withdrawal fees"
    );

    _set_withdrawal_fees(ctx.storage, withdrawal_fees)?;

    Ok(Response::new())
}

fn _set_withdrawal_fees(
    storage: &mut dyn Storage,
    withdrawal_fees: Vec<WithdrawalFee>,
) -> StdResult<()> {
    for WithdrawalFee { denom, remote, fee } in withdrawal_fees {
        match fee {
            Op::Insert(fee) => {
                WITHDRAWAL_FEES.save(storage, (&denom, remote), &fee)?;
            },
            Op::Delete => {
                WITHDRAWAL_FEES.remove(storage, (&denom, remote));
            },
        }
    }

    Ok(())
}

fn set_withdrawal_guardian(ctx: MutableCtx, guardian: Addr) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "only the owner can set the withdrawal guardian"
    );

    WITHDRAWAL_GUARDIAN.save(ctx.storage, &guardian)?;

    Ok(Response::new())
}

fn set_personal_quota(
    ctx: MutableCtx,
    user: Addr,
    denom: Denom,
    quota: Op<SetPersonalQuotaRequest>,
) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "only the owner can set personal quotas"
    );

    match quota {
        Op::Insert(SetPersonalQuotaRequest {
            amount,
            available_for,
        }) => {
            let expire_at = available_for
                .map(|d| ctx.block.timestamp.checked_add(d))
                .transpose()?;

            PERSONAL_QUOTAS.save(
                ctx.storage,
                (user, &denom),
                &PersonalQuota {
                    amount,
                    expire_at,
                    granted_by: ctx.sender,
                    granted_at: ctx.block.timestamp,
                },
            )?;
        },
        Op::Delete => {
            PERSONAL_QUOTAS.remove(ctx.storage, (user, &denom));
        },
    }

    Ok(Response::new())
}

fn receive_remote(
    ctx: MutableCtx,
    remote: Remote,
    amount: Uint128,
    recipient: Addr,
) -> anyhow::Result<Response> {
    // Find the alloyed denom of the given bridge contract and remote.
    let denom = ROUTES.load(ctx.storage, (ctx.sender, remote))?;

    // Increase the reserve only if the denom is remote.
    if denom.is_remote() {
        RESERVES.may_update(ctx.storage, (ctx.sender, remote), |maybe_reserve| {
            let reserve = maybe_reserve.unwrap_or(Uint128::ZERO);

            Ok::<_, StdError>(reserve.checked_add(amount)?)
        })?;
    }

    // First,
    // - if the token is not native on Dango, mint it to the Gateway contract;
    // - otherwise, the token should already been in the Gateway contract, no need
    //   to mint.
    // Then, transfer the token from Gateway to the recipient.
    //
    // Why mint to Gateway first and then transfer to recipient, instead of
    // directly minting to recipient? Because minting doesn't trigger the recipient's
    // `receive` entry point, only transferring does. In some cases, we do need
    // `receive` to be triggered; e.g. activating a new account (see `dango_auth::receive_transfer`).
    Ok(Response::new()
        .may_add_message(if denom.is_remote() {
            let bank = ctx.querier.query_bank()?;
            Some(Message::execute(
                bank,
                &bank::ExecuteMsg::Mint {
                    to: ctx.contract,
                    coins: coins! { denom.clone() => amount },
                },
                Coins::new(),
            )?)
        } else {
            None
        })
        .add_message(Message::transfer(
            recipient,
            coins! { denom.clone() => amount },
        )?)
        .add_event(Deposited {
            user: recipient,
            bridge: ctx.sender,
            remote,
            denom,
            amount,
        })?)
}

fn transfer_remote(ctx: MutableCtx, remote: Remote, recipient: Addr32) -> anyhow::Result<Response> {
    // The user must have sent exactly one coin.
    let coin = ctx.funds.into_one_coin()?;

    // Fail fast if the withdrawal couldn't be executed against the current
    // state: missing route, amount not covering the fee, insufficient
    // reserve, or rate limit exceeded. The resulting plan is discarded —
    // nothing is consumed or recorded until the request is approved, so
    // several pending requests may each pass this validation yet still
    // fail at approval, where it runs again authoritatively.
    validate_withdrawal(ctx.storage, ctx.sender, &coin, remote, ctx.block.timestamp)?;

    // Hold the funds in escrow and store the request. Fees, reserves,
    // personal quotas, and rate limits are all applied if and when the
    // guardian or the owner approves the request.
    let (id, _) = NEXT_WITHDRAWAL_REQUEST_ID.increment(ctx.storage)?;

    WITHDRAWAL_REQUESTS.save(
        ctx.storage,
        id,
        &WithdrawalRequest {
            user: ctx.sender,
            remote,
            recipient,
            coin: coin.clone(),
            status: WithdrawalStatus::Pending,
            created_at: ctx.block.timestamp,
        },
    )?;

    Ok(Response::new().add_event(WithdrawalRequested {
        id,
        user: ctx.sender,
        remote,
        recipient,
        denom: coin.denom,
        amount: coin.amount,
    })?)
}

fn respond_to_withdrawal(
    ctx: MutableCtx,
    id: u64,
    response: WithdrawalResponse,
) -> anyhow::Result<Response> {
    // Look the request up in the pending queue first, then the frozen one.
    // `queue` is the map currently holding it, so the response arms below
    // remove or update the right entry.
    let (request, queue) = if let Some(request) = WITHDRAWAL_REQUESTS.may_load(ctx.storage, id)? {
        (request, &WITHDRAWAL_REQUESTS)
    } else if let Some(request) = FROZEN_WITHDRAWAL_REQUESTS.may_load(ctx.storage, id)? {
        (request, &FROZEN_WITHDRAWAL_REQUESTS)
    } else {
        bail!("withdrawal request not found: {id}");
    };

    let owner = ctx.querier.query_owner()?;
    let guardian = WITHDRAWAL_GUARDIAN.may_load(ctx.storage)?;

    ensure!(
        ctx.sender == owner || Some(ctx.sender) == guardian,
        "you don't have the right, O you don't have the right"
    );

    // A frozen request is escalated to the owner; the guardian can no
    // longer act on it.
    if request.status == WithdrawalStatus::Frozen {
        ensure!(
            ctx.sender == owner,
            "only the owner can respond to a frozen withdrawal request"
        );
    }

    match response {
        WithdrawalResponse::Approve => {
            queue.remove(ctx.storage, id);

            // Validation runs here, not inside `process_withdrawal`: if the
            // withdrawal can no longer be executed (the fee, reserve, or
            // rate limit changed while the request was pending), the escrow
            // is refunded to the user instead of failing the transaction,
            // so an approval always settles the request. Validation writes
            // nothing, so the refund path carries no partial state.
            match validate_withdrawal(
                ctx.storage,
                request.user,
                &request.coin,
                request.remote,
                ctx.block.timestamp,
            ) {
                Ok(plan) => process_withdrawal(ctx, id, request, plan),
                Err(err) => Ok(Response::new()
                    .add_message(Message::transfer(request.user, request.coin.clone())?)
                    .add_event(WithdrawalApprovalFailed {
                        id,
                        user: request.user,
                        denom: request.coin.denom,
                        amount: request.coin.amount,
                        reason: err.to_string(),
                    })?),
            }
        },
        WithdrawalResponse::Reject => {
            queue.remove(ctx.storage, id);

            // Refund the full escrowed amount to the user; no fee is
            // charged on a rejected withdrawal.
            Ok(Response::new()
                .add_message(Message::transfer(request.user, request.coin.clone())?)
                .add_event(WithdrawalRejected {
                    id,
                    user: request.user,
                    denom: request.coin.denom,
                    amount: request.coin.amount,
                    rejected_by: ctx.sender,
                })?)
        },
        WithdrawalResponse::Freeze => {
            ensure!(
                request.status == WithdrawalStatus::Pending,
                "withdrawal request is already frozen: {id}"
            );

            // Move the request out of the guardian's queue into the owner's.
            WITHDRAWAL_REQUESTS.remove(ctx.storage, id);
            FROZEN_WITHDRAWAL_REQUESTS.save(
                ctx.storage,
                id,
                &WithdrawalRequest {
                    status: WithdrawalStatus::Frozen,
                    ..request.clone()
                },
            )?;

            Ok(Response::new().add_event(WithdrawalFrozen {
                id,
                user: request.user,
                denom: request.coin.denom,
                amount: request.coin.amount,
                frozen_by: ctx.sender,
            })?)
        },
        WithdrawalResponse::Confiscate => {
            ensure!(
                ctx.sender == owner,
                "only the owner can confiscate a withdrawal request"
            );

            ensure!(
                request.status == WithdrawalStatus::Frozen,
                "only a frozen withdrawal request can be confiscated: {id}"
            );

            FROZEN_WITHDRAWAL_REQUESTS.remove(ctx.storage, id);

            Ok(Response::new()
                .add_message(Message::transfer(owner, request.coin.clone())?)
                .add_event(WithdrawalConfiscated {
                    id,
                    user: request.user,
                    denom: request.coin.denom,
                    amount: request.coin.amount,
                })?)
        },
    }
}

/// Execute an approved withdrawal request: apply the state updates computed
/// by [`validate_withdrawal`] (reserve, personal quota, rolling window) and
/// dispatch the transfer to the bridge contract.
///
/// The caller is responsible for running [`validate_withdrawal`] on the
/// same block and passing the resulting plan in; keeping validation outside
/// lets the approval handler refund the escrow when validation fails,
/// rather than failing the transaction.
fn process_withdrawal(
    ctx: MutableCtx,
    id: u64,
    request: WithdrawalRequest,
    plan: WithdrawalPlan,
) -> anyhow::Result<Response> {
    let WithdrawalRequest {
        user,
        remote,
        recipient,
        mut coin,
        ..
    } = request;

    // Validation passed — apply the state updates it computed.
    if let Some(new_reserve) = plan.new_reserve {
        RESERVES.save(ctx.storage, (plan.bridge, remote), &new_reserve)?;
    }

    match plan.personal_quota_update {
        Some(Op::Insert(pq)) => PERSONAL_QUOTAS.save(ctx.storage, (user, &coin.denom), &pq)?,
        Some(Op::Delete) => PERSONAL_QUOTAS.remove(ctx.storage, (user, &coin.denom)),
        None => (),
    }

    rate_limit::record(ctx.storage, &coin.denom, ctx.block.timestamp, plan.residue)?;

    // From here on, `coin` is the post-fee amount actually bridged.
    let bridge = plan.bridge;
    let maybe_fee = plan.fee;
    coin.amount = plan.net_amount;

    let (bank, owner) = ctx.querier.query_bank_and_owner()?;

    // 1. Call the bridge contract to make the remote transfer.
    // 2. Burn the alloyed token to be transferred (only if the token is not native on Dango).
    // 3. Send the withdrawal fee to the chain owner.
    Ok(Response::new()
        .add_message(Message::execute(
            bridge,
            &bridge::ExecuteMsg::Bridge(BridgeMsg::TransferRemote {
                remote,
                amount: coin.amount,
                recipient,
            }),
            Coins::new(),
        )?)
        .may_add_message(if coin.denom.is_remote() {
            Some(Message::execute(
                bank,
                &bank::ExecuteMsg::Burn {
                    from: ctx.contract,
                    coins: coin.clone().into(),
                },
                Coins::new(),
            )?)
        } else {
            None
        })
        .may_add_message(if let Some(fee) = maybe_fee {
            Some(Message::transfer(
                owner,
                coins! { coin.denom.clone() => fee },
            )?)
        } else {
            None
        })
        .add_event(Withdrawn {
            id,
            user,
            bridge,
            remote,
            recipient,
            denom: coin.denom,
            amount: coin.amount,
            fee: maybe_fee.unwrap_or(Uint128::ZERO),
        })?)
}

/// The state updates a withdrawal entails, precomputed by
/// [`validate_withdrawal`]. Everything here is derived read-only; applying
/// it is infallible, so a withdrawal either fails validation with no
/// storage written, or executes in full.
struct WithdrawalPlan {
    /// The bridge contract handling the (denom, remote) route.
    bridge: Addr,
    /// The withdrawal fee charged, if one is configured for the route.
    fee: Option<Uint128>,
    /// The amount bridged to the remote chain: the escrowed amount minus
    /// the fee.
    net_amount: Uint128,
    /// The new reserve for (bridge, remote) after deducting the bridged
    /// amount. `None` when the denom is local-origin, for which reserves
    /// aren't tracked.
    new_reserve: Option<Uint128>,
    /// The update to the user's personal quota after consuming it:
    /// `Op::Delete` when fully consumed, `Op::Insert` with the leftover
    /// otherwise. `None` when the user has no active quota.
    personal_quota_update: Option<Op<PersonalQuota>>,
    /// The post-personal-quota residue counting against the trailing-24h
    /// rolling window.
    residue: Uint128,
}

/// Validate a withdrawal of `coin` by `user` against the current state:
///
/// - a route must exist for the (denom, remote) tuple;
/// - the escrowed amount must exceed the withdrawal fee, leaving a
///   non-zero amount to bridge;
/// - the route's reserve must cover the bridged amount (remote denoms only);
/// - the post-personal-quota residue must fit the rate-limit headroom.
///
/// Reads storage but never writes it. On success, returns the
/// [`WithdrawalPlan`] with every update the withdrawal entails; the caller
/// decides whether to apply it (approval) or discard it (the fail-fast
/// validation when the request is created).
fn validate_withdrawal(
    storage: &dyn Storage,
    user: Addr,
    coin: &Coin,
    remote: Remote,
    now: Timestamp,
) -> anyhow::Result<WithdrawalPlan> {
    // Find the bridge contract corresponding to the (denom, remote) tuple.
    let bridge = REVERSE_ROUTES.load(storage, (&coin.denom, remote))?;

    // Deduct the withdrawal fee.
    let fee = WITHDRAWAL_FEES.may_load(storage, (&coin.denom, remote))?;

    let bridged = match fee {
        Some(fee) => {
            // Strictly greater: an amount equal to the fee would leave zero
            // to bridge, and a zero-amount burn or bridge message fails
            // downstream (the bank rejects zero-amount coins).
            ensure!(
                coin.amount > fee,
                "withdrawal amount not sufficient to cover fee: {} <= {}",
                coin.amount,
                fee
            );

            coin.amount.checked_sub(fee)?
        },
        None => coin.amount,
    };

    // The reserve must cover the bridged amount, but only remote denoms
    // track a reserve.
    let new_reserve = if coin.denom.is_remote() {
        let reserve = RESERVES
            .may_load(storage, (bridge, remote))?
            .unwrap_or(Uint128::ZERO);

        Some(reserve.checked_sub(bridged).map_err(|_| {
            anyhow!(
                "insufficient reserve! bridge: {}, remote: {:?}, reserve: {}, amount: {}",
                bridge,
                remote,
                reserve,
                bridged
            )
        })?)
    } else {
        None
    };

    // Consume the user's personal quota first, if any and still active.
    // Whatever is left over falls through to the global outbound quota.
    let mut residue = bridged;

    let personal_quota = match active_personal_quota(storage, (user, &coin.denom), now)? {
        Some(pq) => {
            let consumed = pq.amount.min(residue);
            residue = residue.checked_sub(consumed)?;

            let leftover = pq.amount.checked_sub(consumed)?;
            Some(if leftover.is_zero() {
                Op::Delete
            } else {
                Op::Insert(PersonalQuota {
                    amount: leftover,
                    ..pq
                })
            })
        },
        None => None,
    };

    // Check the trailing-24h rolling window against the cap.
    rate_limit::check(storage, &coin.denom, now, bridged, residue)?;

    Ok(WithdrawalPlan {
        bridge,
        fee,
        net_amount: bridged,
        new_reserve,
        personal_quota_update: personal_quota,
        residue,
    })
}

/// Load the user's personal quota, if one exists and is still active at
/// `now`. An expired entry is treated as absent (it is left in storage;
/// scrubbing is the consumer's concern).
fn active_personal_quota(
    storage: &dyn Storage,
    key: (Addr, &Denom),
    now: Timestamp,
) -> StdResult<Option<PersonalQuota>> {
    Ok(PERSONAL_QUOTAS
        .may_load(storage, key)?
        .filter(|pq| pq.expire_at.is_none_or(|t| now < t)))
}

pub fn cron_execute(ctx: SudoCtx) -> StdResult<Response> {
    rate_limit::tick(ctx.storage, ctx.querier, ctx.block.timestamp)?;

    Ok(Response::new())
}
