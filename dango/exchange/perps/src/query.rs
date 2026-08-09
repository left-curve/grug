use {
    crate::{
        core::{
            compute_available_margin, compute_liquidation_price, compute_maintenance_margin,
            compute_position_unrealized_funding, compute_position_unrealized_pnl,
            compute_user_equity,
        },
        querier::NoCachePerpQuerier,
        referral::calculate_commission_rate,
        state::{
            COMMISSION_RATE_OVERRIDES, FEE_RATE_OVERRIDES, FEE_SHARE_RATIO, PAIR_PARAMS,
            PAIR_STATES, REFEREE_TO_REFERRER, REFERRER_TO_REFEREE_STATISTICS, USER_REFERRAL_DATA,
            USER_STATES, VAULT_SNAPSHOTS,
        },
    },
    anyhow::ensure,
    dango_order_book::{
        ASKS, BIDS, ClientOrderId, DEPTHS, Dimensionless, LimitOrder, LiquidityDepth,
        LiquidityDepthResponse, OrderId, PairId, QueryOrderByClientOrderIdResponse,
        QueryOrderResponse, QueryOrdersByUserResponseItem, UsdPrice, UsdValue, VOLUMES,
        round_to_day,
    },
    dango_primitives::{
        Addr, Bound, DEFAULT_PAGE_LIMIT, ImmutableCtx, Order as IterationOrder, StdResult, Storage,
        Timestamp,
    },
    dango_storage::{MultiIndex, PrefixBound, PrimaryKey, StorageQuerier},
    dango_types::{
        account_factory::UserIndex,
        perps::{
            CommissionRate, PairParam, PairState, PositionExtended, Referee, RefereeStats,
            Referrer, ReferrerSettings, ReferrerStatsOrderBy, ReferrerStatsOrderIndex,
            UserReferralData, UserState, UserStateExtended, VaultSnapshot,
        },
    },
    std::collections::BTreeMap,
};

pub fn query_pair_params(
    ctx: ImmutableCtx,
    start_after: Option<PairId>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<PairId, PairParam>> {
    let start = start_after.as_ref().map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    PAIR_PARAMS
        .range(ctx.storage, start, None, IterationOrder::Ascending)
        .take(limit)
        .collect()
}

pub fn query_pair_states(
    ctx: ImmutableCtx,
    start_after: Option<PairId>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<PairId, PairState>> {
    let start = start_after.as_ref().map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    PAIR_STATES
        .range(ctx.storage, start, None, IterationOrder::Ascending)
        .take(limit)
        .collect()
}

pub fn query_user_states(
    ctx: ImmutableCtx,
    start_after: Option<Addr>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<Addr, UserState>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    USER_STATES
        .range(ctx.storage, start, None, IterationOrder::Ascending)
        .take(limit)
        .collect()
}

pub fn query_user_state_extended(
    storage: &dyn Storage,
    user: Addr,
    include_equity: bool,
    include_available_margin: bool,
    include_maintenance_margin: bool,
    include_unrealized_pnl: bool,
    include_unrealized_funding: bool,
    include_liquidation_price: bool,
    include_all: bool,
) -> anyhow::Result<UserStateExtended> {
    let user_state = USER_STATES.load(storage, user)?;

    let perp_querier = NoCachePerpQuerier::new_local(storage);

    let equity = if include_all || include_equity {
        Some(compute_user_equity(&perp_querier, &user_state)?)
    } else {
        None
    };

    let available_margin = if include_all || include_available_margin {
        Some(compute_available_margin(&perp_querier, &user_state)?)
    } else {
        None
    };

    let maintenance_margin = if include_all || include_maintenance_margin {
        Some(compute_maintenance_margin(&perp_querier, &user_state)?)
    } else {
        None
    };

    let positions = user_state
        .positions
        .iter()
        .map(|(pair_id, position)| {
            let unrealized_pnl = if include_all || include_unrealized_pnl {
                let index_price = PAIR_STATES.load(storage, pair_id)?.index_price;
                Some(compute_position_unrealized_pnl(position, index_price)?)
            } else {
                None
            };

            let unrealized_funding = if include_all || include_unrealized_funding {
                let pair_state = perp_querier.query_pair_state(pair_id)?;
                Some(compute_position_unrealized_funding(position, &pair_state)?)
            } else {
                None
            };

            let liquidation_price = if include_all || include_liquidation_price {
                compute_liquidation_price(pair_id, &user_state, &perp_querier)?
            } else {
                None
            };

            Ok((
                pair_id.clone(),
                PositionExtended {
                    size: position.size,
                    entry_price: position.entry_price,
                    entry_funding_per_unit: position.entry_funding_per_unit,
                    conditional_order_above: position.conditional_order_above.clone(),
                    conditional_order_below: position.conditional_order_below.clone(),
                    unrealized_pnl,
                    unrealized_funding,
                    liquidation_price,
                },
            ))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;

    Ok(UserStateExtended {
        margin: user_state.margin,
        vault_shares: user_state.vault_shares,
        unlocks: user_state.unlocks,
        reserved_margin: user_state.reserved_margin,
        open_order_count: user_state.open_order_count,
        equity,
        available_margin,
        maintenance_margin,
        positions,
    })
}

pub fn query_user_states_extended(
    storage: &dyn Storage,
    start_after: Option<Addr>,
    limit: Option<u32>,
    include_equity: bool,
    include_available_margin: bool,
    include_maintenance_margin: bool,
    include_unrealized_pnl: bool,
    include_unrealized_funding: bool,
    include_liquidation_price: bool,
    include_all: bool,
) -> anyhow::Result<BTreeMap<Addr, UserStateExtended>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    USER_STATES
        .keys(storage, start, None, IterationOrder::Ascending)
        .take(limit)
        .map(|res| {
            let user = res?;
            let user_state = query_user_state_extended(
                storage,
                user,
                include_equity,
                include_available_margin,
                include_maintenance_margin,
                include_unrealized_pnl,
                include_unrealized_funding,
                include_liquidation_price,
                include_all,
            )?;
            Ok((user, user_state))
        })
        .collect()
}

/// Search `BIDS` and `ASKS` for an order with the given ID.
pub fn query_order(ctx: ImmutableCtx, order_id: OrderId) -> StdResult<Option<QueryOrderResponse>> {
    // Check `BIDS` (un-invert price).
    if let Some(((pair_id, stored_price, _), order)) =
        BIDS.idx.order_id.may_load(ctx.storage, order_id)?
    {
        return Ok(Some(limit_order_to_response(pair_id, !stored_price, order)));
    }

    // Check `ASKS` (price as-is).
    if let Some(((pair_id, limit_price, _), order)) =
        ASKS.idx.order_id.may_load(ctx.storage, order_id)?
    {
        return Ok(Some(limit_order_to_response(pair_id, limit_price, order)));
    }

    Ok(None)
}

/// Return all limit orders for a user, keyed by order ID.
pub fn query_orders_by_user(
    ctx: ImmutableCtx,
    user: Addr,
) -> StdResult<BTreeMap<OrderId, QueryOrdersByUserResponseItem>> {
    let mut items = BTreeMap::new();

    // `BIDS` (un-invert price).
    for res in BIDS
        .idx
        .user
        .prefix(user)
        .range(ctx.storage, None, None, IterationOrder::Ascending)
    {
        let ((pair_id, stored_price, order_id), order) = res?;
        items.insert(order_id, limit_order_to_item(pair_id, !stored_price, order));
    }

    // `ASKS` (price as-is).
    for res in ASKS
        .idx
        .user
        .prefix(user)
        .range(ctx.storage, None, None, IterationOrder::Ascending)
    {
        let ((pair_id, limit_price, order_id), order) = res?;
        items.insert(order_id, limit_order_to_item(pair_id, limit_price, order));
    }

    Ok(items)
}

/// Search `BIDS` and `ASKS` for an order with the given submitter and client
/// order ID, via the `(user, client_order_id)` unique index.
pub fn query_order_by_client_order_id(
    ctx: ImmutableCtx,
    user: Addr,
    client_order_id: ClientOrderId,
) -> StdResult<Option<QueryOrderByClientOrderIdResponse>> {
    // Check `BIDS` (un-invert price).
    if let Some(((pair_id, stored_price, order_id), order)) = BIDS
        .idx
        .client_order_id
        .may_load(ctx.storage, (user, client_order_id))?
    {
        return Ok(Some(limit_order_to_cid_response(
            pair_id,
            !stored_price,
            order_id,
            order,
        )));
    }

    // Check `ASKS` (price as-is).
    if let Some(((pair_id, limit_price, order_id), order)) = ASKS
        .idx
        .client_order_id
        .may_load(ctx.storage, (user, client_order_id))?
    {
        return Ok(Some(limit_order_to_cid_response(
            pair_id,
            limit_price,
            order_id,
            order,
        )));
    }

    Ok(None)
}

fn limit_order_to_response(
    pair_id: PairId,
    limit_price: UsdPrice,
    order: LimitOrder,
) -> QueryOrderResponse {
    QueryOrderResponse {
        user: order.user,
        pair_id,
        size: order.size,
        limit_price,
        reduce_only: order.reduce_only,
        reserved_margin: order.reserved_margin,
        created_at: order.created_at,
        tp: order.tp,
        sl: order.sl,
        client_order_id: order.client_order_id,
    }
}

fn limit_order_to_cid_response(
    pair_id: PairId,
    limit_price: UsdPrice,
    order_id: OrderId,
    order: LimitOrder,
) -> QueryOrderByClientOrderIdResponse {
    QueryOrderByClientOrderIdResponse {
        order_id,
        pair_id,
        size: order.size,
        limit_price,
        reduce_only: order.reduce_only,
        reserved_margin: order.reserved_margin,
        created_at: order.created_at,
        tp: order.tp,
        sl: order.sl,
    }
}

fn limit_order_to_item(
    pair_id: PairId,
    limit_price: UsdPrice,
    order: LimitOrder,
) -> QueryOrdersByUserResponseItem {
    QueryOrdersByUserResponseItem {
        pair_id,
        size: order.size,
        limit_price,
        reduce_only: order.reduce_only,
        reserved_margin: order.reserved_margin,
        created_at: order.created_at,
        tp: order.tp,
        sl: order.sl,
        client_order_id: order.client_order_id,
    }
}

pub fn query_liquidity_depth(
    ctx: ImmutableCtx,
    pair_id: PairId,
    bucket_size: UsdPrice,
    limit: Option<u32>,
) -> anyhow::Result<LiquidityDepthResponse> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;
    let pair_param = PAIR_PARAMS.load(ctx.storage, &pair_id)?;

    ensure!(
        pair_param.bucket_sizes.contains(&bucket_size),
        "bucket size {bucket_size} not configured for pair {pair_id}"
    );

    let bids = DEPTHS
        .prefix(&pair_id)
        .append(bucket_size)
        .append(true)
        .range(ctx.storage, None, None, IterationOrder::Descending)
        .take(limit)
        .map(|res| {
            let (bucket, (size, notional)) = res?;
            Ok((bucket, LiquidityDepth { size, notional }))
        })
        .collect::<StdResult<_>>()?;

    let asks = DEPTHS
        .prefix(&pair_id)
        .append(bucket_size)
        .append(false)
        .range(ctx.storage, None, None, IterationOrder::Ascending)
        .take(limit)
        .map(|res| {
            let (bucket, (size, notional)) = res?;
            Ok((bucket, LiquidityDepth { size, notional }))
        })
        .collect::<StdResult<_>>()?;

    Ok(LiquidityDepthResponse { bids, asks })
}

pub fn query_volume(
    storage: &dyn Storage,
    user: Addr,
    since: Option<Timestamp>,
) -> StdResult<UsdValue> {
    let latest = VOLUMES
        .prefix(user)
        .range(storage, None, None, IterationOrder::Descending)
        .next()
        .transpose()?
        .map(|(_, v)| v)
        .unwrap_or(UsdValue::ZERO);

    match since {
        None => Ok(latest),
        Some(ts) => {
            let day = round_to_day(ts);
            let baseline = VOLUMES
                .prefix(user)
                .range(
                    storage,
                    None,
                    Some(Bound::Inclusive(day)),
                    IterationOrder::Descending,
                )
                .next()
                .transpose()?
                .map(|(_, v)| v)
                .unwrap_or(UsdValue::ZERO);
            Ok(latest.checked_sub(baseline)?)
        },
    }
}

pub fn query_volume_by_user(
    ctx: ImmutableCtx,
    user: UserIndex,
    since: Option<Timestamp>,
) -> anyhow::Result<UsdValue> {
    let account_factory = crate::account_factory(ctx.querier);
    compute_user_volume(ctx.storage, ctx.querier, account_factory, user, since)
}

/// Sum cumulative volume across all accounts belonging to a user.
pub fn compute_user_volume(
    storage: &dyn Storage,
    querier: impl StorageQuerier,
    account_factory: Addr,
    user: UserIndex,
    since: Option<Timestamp>,
) -> anyhow::Result<UsdValue> {
    let user_data =
        querier.query_wasm_path(account_factory, &dango_account_factory::USERS.path(user))?;

    let mut total = UsdValue::ZERO;
    for addr in user_data.accounts.values() {
        total = total.checked_add(query_volume(storage, *addr, since)?)?;
    }

    Ok(total)
}

pub fn query_vault_snapshots(
    storage: &dyn Storage,
    min: Option<Timestamp>,
    max: Option<Timestamp>,
) -> StdResult<BTreeMap<Timestamp, VaultSnapshot>> {
    let min = min.map(Bound::Inclusive);
    let max = max.map(Bound::Inclusive);

    VAULT_SNAPSHOTS
        .range(storage, min, max, IterationOrder::Ascending)
        .collect()
}

pub fn query_referrer(storage: &dyn Storage, referee: UserIndex) -> StdResult<Option<Referrer>> {
    REFEREE_TO_REFERRER.may_load(storage, referee)
}

pub fn query_commission_rate_override(
    storage: &dyn Storage,
    user: UserIndex,
) -> StdResult<Option<CommissionRate>> {
    COMMISSION_RATE_OVERRIDES.may_load(storage, user)
}

pub fn query_commission_rate_overrides(
    ctx: ImmutableCtx,
    start_after: Option<UserIndex>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<UserIndex, CommissionRate>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    COMMISSION_RATE_OVERRIDES
        .range(ctx.storage, start, None, IterationOrder::Ascending)
        .take(limit)
        .collect()
}

pub fn query_referral_data(
    ctx: ImmutableCtx,
    user: UserIndex,
    since: Option<Timestamp>,
) -> anyhow::Result<UserReferralData> {
    let Some(data_now) = USER_REFERRAL_DATA
        .prefix(user)
        .values(ctx.storage, None, None, IterationOrder::Descending)
        .next()
        .transpose()?
    else {
        return Ok(UserReferralData::default());
    };

    let data_since = if let Some(since) = since {
        USER_REFERRAL_DATA
            .prefix(user)
            .values(
                ctx.storage,
                None,
                Some(Bound::Inclusive(since)),
                IterationOrder::Descending,
            )
            .next()
            .transpose()?
            .unwrap_or_default()
    } else {
        UserReferralData::default()
    };

    Ok(data_now.checked_sub(&data_since)?)
}

pub fn query_referral_data_entries(
    ctx: ImmutableCtx,
    user: UserIndex,
    start_after: Option<Timestamp>,
    limit: Option<u32>,
) -> StdResult<Vec<(Timestamp, UserReferralData)>> {
    let max = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    USER_REFERRAL_DATA
        .prefix(user)
        .range(ctx.storage, None, max, IterationOrder::Descending)
        .take(limit)
        .collect()
}

pub fn query_referrer_to_referee_stats(
    ctx: ImmutableCtx,
    referrer: Referrer,
    order_by: ReferrerStatsOrderBy,
) -> StdResult<Vec<(Referee, RefereeStats)>> {
    let limit = order_by.limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    match order_by.index {
        ReferrerStatsOrderIndex::Commission { start_after } => collect_referee_stats(
            ctx.storage,
            &REFERRER_TO_REFEREE_STATISTICS.idx.commission,
            referrer,
            start_after,
            limit,
            order_by.order,
        ),
        ReferrerStatsOrderIndex::RegisterAt { start_after } => collect_referee_stats(
            ctx.storage,
            &REFERRER_TO_REFEREE_STATISTICS.idx.registered_at,
            referrer,
            start_after,
            limit,
            order_by.order,
        ),
        ReferrerStatsOrderIndex::Volume { start_after } => collect_referee_stats(
            ctx.storage,
            &REFERRER_TO_REFEREE_STATISTICS.idx.volume,
            referrer,
            start_after,
            limit,
            order_by.order,
        ),
    }
}

fn collect_referee_stats<'a, S>(
    storage: &dyn Storage,
    index: &MultiIndex<'a, (Referrer, Referee), (Referrer, S), RefereeStats>,
    referrer: Referrer,
    start_after: Option<S>,
    limit: usize,
    order: IterationOrder,
) -> StdResult<Vec<(Referee, RefereeStats)>>
where
    S: PrimaryKey,
{
    let start_after = start_after.map(PrefixBound::Exclusive);

    let (min, max) = match order {
        IterationOrder::Ascending => (start_after, None),
        IterationOrder::Descending => (None, start_after),
    };

    index
        .sub_prefix(referrer)
        .prefix_range(storage, min, max, order)
        .take(limit)
        .map(|value| {
            let ((_, referee), referee_stats) = value?;
            Ok((referee, referee_stats))
        })
        .collect()
}

pub fn query_referral_settings(
    ctx: ImmutableCtx,
    user: UserIndex,
) -> anyhow::Result<Option<ReferrerSettings>> {
    let Some(share_ratio) = FEE_SHARE_RATIO.may_load(ctx.storage, user)? else {
        return Ok(None);
    };

    let param = crate::PARAM.load(ctx.storage)?;

    let commission_rate =
        calculate_commission_rate(ctx.storage, user, ctx.block.timestamp, &param)?;

    Ok(Some(ReferrerSettings {
        commission_rate,
        share_ratio,
    }))
}

pub fn query_fee_rate_overrides(
    ctx: ImmutableCtx,
    start_after: Option<Addr>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<Addr, (Dimensionless, Dimensionless)>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    FEE_RATE_OVERRIDES
        .range(ctx.storage, start, None, IterationOrder::Ascending)
        .take(limit)
        .collect()
}
