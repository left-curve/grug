use {
    crate::{POSITIONS, UNLOCKING_SCHEDULE},
    dango_primitives::{
        Addr, Bound, DEFAULT_PAGE_LIMIT, ImmutableCtx, Json, JsonSerExt, Order, StdResult,
    },
    dango_types::vesting::{PositionResponse, QueryMsg},
    std::collections::BTreeMap,
};

pub fn query(ctx: ImmutableCtx, msg: QueryMsg) -> StdResult<Json> {
    match msg {
        QueryMsg::Position { user } => {
            let res = query_position(ctx, user)?;
            res.to_json_value()
        },
        QueryMsg::Positions { start_after, limit } => {
            let res = query_positions(ctx, start_after, limit)?;
            res.to_json_value()
        },
    }
}

fn query_position(ctx: ImmutableCtx, user: Addr) -> StdResult<PositionResponse> {
    let unlocking_schedule = UNLOCKING_SCHEDULE.load(ctx.storage)?;
    let position = POSITIONS.load(ctx.storage, user)?;
    let claimable = position.compute_claimable(ctx.block.timestamp, &unlocking_schedule)?;

    Ok(PositionResponse {
        position,
        claimable,
    })
}

fn query_positions(
    ctx: ImmutableCtx,
    start_after: Option<Addr>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<Addr, PositionResponse>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT) as usize;
    let unlocking_schedule = UNLOCKING_SCHEDULE.load(ctx.storage)?;

    POSITIONS
        .range(ctx.storage, start, None, Order::Ascending)
        .take(limit)
        .map(|res| {
            let (user, position) = res?;
            let claimable = position.compute_claimable(ctx.block.timestamp, &unlocking_schedule)?;

            Ok((
                user,
                PositionResponse {
                    position,
                    claimable,
                },
            ))
        })
        .collect()
}
