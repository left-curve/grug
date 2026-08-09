use {
    crate::{
        FROZEN_WITHDRAWAL_REQUESTS, PERSONAL_QUOTAS, RESERVES, REVERSE_ROUTES, ROUTES,
        WITHDRAWAL_FEES, WITHDRAWAL_GUARDIAN, WITHDRAWAL_REQUESTS, rate_limit,
    },
    dango_math::Uint128,
    dango_primitives::{
        Addr, Bound, DEFAULT_PAGE_LIMIT, Denom, ImmutableCtx, Json, JsonSerExt, Order, StdResult,
    },
    dango_types::gateway::{
        PersonalQuota, QueryMsg, QueryPersonalQuotasResponseItem, QueryReservesResponseItem,
        QueryRoutesResponseItem, QueryWithdrawalFeesResponseItem,
        QueryWithdrawalRequestsResponseItem, Remote, WithdrawalRequest,
    },
};

pub fn query(ctx: ImmutableCtx, msg: QueryMsg) -> StdResult<Json> {
    match msg {
        QueryMsg::Route { bridge, remote } => {
            let res = query_route(ctx, bridge, remote)?;
            res.to_json_value()
        },
        QueryMsg::ReverseRoute { denom, remote } => {
            let res = query_reverse_route(ctx, denom, remote)?;
            res.to_json_value()
        },
        QueryMsg::Routes { start_after, limit } => {
            let res = query_routes(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::Reserve { bridge, remote } => {
            let res = query_reserve(ctx, bridge, remote)?;
            res.to_json_value()
        },
        QueryMsg::Reserves { start_after, limit } => {
            let res = query_reserves(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::WithdrawalFee { denom, remote } => {
            let res = query_withdrawal_fee(ctx, denom, remote)?;
            res.to_json_value()
        },
        QueryMsg::WithdrawalFees { start_after, limit } => {
            let res = query_withdrawal_fees(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::PersonalQuota { user, denom } => {
            let res = query_personal_quota(ctx, user, denom)?;
            res.to_json_value()
        },
        QueryMsg::PersonalQuotas { start_after, limit } => {
            let res = query_personal_quotas(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::Guardian {} => {
            let res = query_withdrawal_guardian(ctx)?;
            res.to_json_value()
        },
        QueryMsg::WithdrawalRequest { id } => {
            let res = query_withdrawal_request(ctx, id)?;
            res.to_json_value()
        },
        QueryMsg::WithdrawalRequests { start_after, limit } => {
            let res = query_withdrawal_requests(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::FrozenWithdrawalRequests { start_after, limit } => {
            let res = query_frozen_withdrawal_requests(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::RateLimits {} => {
            let res = rate_limit::query_rate_limits(ctx)?;
            res.to_json_value()
        },
        QueryMsg::RateLimitStatus { denom } => {
            let res = rate_limit::query_rate_limit_status(ctx, denom)?;
            res.to_json_value()
        },
        QueryMsg::RateLimitStatuses { start_after, limit } => {
            let res = rate_limit::query_rate_limit_statuses(ctx, start_after, limit)?;
            res.to_json_value()
        },
    }
}

fn query_route(ctx: ImmutableCtx, bridge: Addr, remote: Remote) -> StdResult<Option<Denom>> {
    ROUTES.may_load(ctx.storage, (bridge, remote))
}

fn query_reverse_route(ctx: ImmutableCtx, denom: Denom, remote: Remote) -> StdResult<Option<Addr>> {
    REVERSE_ROUTES.may_load(ctx.storage, (&denom, remote))
}

fn query_routes(
    ctx: ImmutableCtx,
    start_after: Option<(Addr, Remote)>,
    limit: Option<u32>,
) -> StdResult<Vec<QueryRoutesResponseItem>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    ROUTES
        .range(ctx.storage, start, None, Order::Ascending)
        .map(|res| {
            let ((bridge, remote), denom) = res?;
            Ok(QueryRoutesResponseItem {
                bridge,
                remote,
                denom,
            })
        })
        .take(limit)
        .collect()
}

fn query_reserve(ctx: ImmutableCtx, bridge: Addr, remote: Remote) -> StdResult<Uint128> {
    RESERVES.load(ctx.storage, (bridge, remote))
}

fn query_reserves(
    ctx: ImmutableCtx,
    start_after: Option<(Addr, Remote)>,
    limit: Option<u32>,
) -> StdResult<Vec<QueryReservesResponseItem>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    RESERVES
        .range(ctx.storage, start, None, Order::Ascending)
        .map(|res| {
            let ((bridge, remote), reserve) = res?;
            Ok(QueryReservesResponseItem {
                bridge,
                remote,
                reserve,
            })
        })
        .take(limit)
        .collect()
}

fn query_withdrawal_fee(ctx: ImmutableCtx, denom: Denom, remote: Remote) -> StdResult<Uint128> {
    WITHDRAWAL_FEES.load(ctx.storage, (&denom, remote))
}

fn query_withdrawal_fees(
    ctx: ImmutableCtx,
    start_after: Option<(Denom, Remote)>,
    limit: Option<u32>,
) -> StdResult<Vec<QueryWithdrawalFeesResponseItem>> {
    let start = start_after
        .as_ref()
        .map(|(denom, remote)| Bound::Exclusive((denom, *remote)));
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    WITHDRAWAL_FEES
        .range(ctx.storage, start, None, Order::Ascending)
        .map(|res| {
            let ((denom, remote), fee) = res?;
            Ok(QueryWithdrawalFeesResponseItem { denom, remote, fee })
        })
        .take(limit)
        .collect()
}

fn query_withdrawal_guardian(ctx: ImmutableCtx) -> StdResult<Option<Addr>> {
    WITHDRAWAL_GUARDIAN.may_load(ctx.storage)
}

fn query_withdrawal_request(ctx: ImmutableCtx, id: u64) -> StdResult<Option<WithdrawalRequest>> {
    if let Some(request) = WITHDRAWAL_REQUESTS.may_load(ctx.storage, id)? {
        return Ok(Some(request));
    }

    FROZEN_WITHDRAWAL_REQUESTS.may_load(ctx.storage, id)
}

fn query_withdrawal_requests(
    ctx: ImmutableCtx,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<Vec<QueryWithdrawalRequestsResponseItem>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    WITHDRAWAL_REQUESTS
        .range(ctx.storage, start, None, Order::Ascending)
        .map(|res| {
            let (id, request) = res?;
            Ok(QueryWithdrawalRequestsResponseItem { id, request })
        })
        .take(limit)
        .collect()
}

fn query_frozen_withdrawal_requests(
    ctx: ImmutableCtx,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<Vec<QueryWithdrawalRequestsResponseItem>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    FROZEN_WITHDRAWAL_REQUESTS
        .range(ctx.storage, start, None, Order::Ascending)
        .map(|res| {
            let (id, request) = res?;
            Ok(QueryWithdrawalRequestsResponseItem { id, request })
        })
        .take(limit)
        .collect()
}

fn query_personal_quota(
    ctx: ImmutableCtx,
    user: Addr,
    denom: Denom,
) -> StdResult<Option<PersonalQuota>> {
    PERSONAL_QUOTAS.may_load(ctx.storage, (user, &denom))
}

fn query_personal_quotas(
    ctx: ImmutableCtx,
    start_after: Option<(Addr, Denom)>,
    limit: Option<u32>,
) -> StdResult<Vec<QueryPersonalQuotasResponseItem>> {
    let start = start_after
        .as_ref()
        .map(|(user, denom)| Bound::Exclusive((*user, denom)));
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;

    PERSONAL_QUOTAS
        .range(ctx.storage, start, None, Order::Ascending)
        .map(|res| {
            let ((user, denom), quota) = res?;
            Ok(QueryPersonalQuotasResponseItem { user, denom, quota })
        })
        .take(limit)
        .collect()
}
