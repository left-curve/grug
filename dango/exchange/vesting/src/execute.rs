use {
    crate::{POSITIONS, UNLOCKING_SCHEDULE},
    anyhow::{bail, ensure},
    dango_math::{IsZero, Number, NumberConst, Uint128},
    dango_primitives::{Addr, Coin, Message, MutableCtx, QuerierExt, Response},
    dango_types::{
        constants::dango,
        vesting::{ExecuteMsg, InstantiateMsg, Position, Schedule, VestingStatus},
    },
};

pub fn instantiate(ctx: MutableCtx, msg: InstantiateMsg) -> anyhow::Result<Response> {
    UNLOCKING_SCHEDULE.save(
        ctx.storage,
        &Schedule {
            // Unlocking start time is defined as the token generation time.
            // Since this contract is to be deployed at genesis, and the token is
            // generated also at genesis, the start time is simply the block time.
            start_time: ctx.block.timestamp,
            cliff: msg.unlocking_cliff,
            period: msg.unlocking_period,
        },
    )?;

    Ok(Response::new())
}

pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> anyhow::Result<Response> {
    match msg {
        ExecuteMsg::Create { user, schedule } => create(ctx, user, schedule),
        ExecuteMsg::Terminate { user } => terminate(ctx, user),
        ExecuteMsg::Claim {} => claim(ctx),
    }
}

fn create(ctx: MutableCtx, user: Addr, schedule: Schedule) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "you don't have the right, O you don't have the right"
    );

    let coin = ctx.funds.into_one_coin_of_denom(&dango::DENOM)?;

    POSITIONS.save(
        ctx.storage,
        user,
        &Position {
            vesting_status: VestingStatus::Active(schedule),
            total: coin.amount,
            claimed: Uint128::ZERO,
        },
    )?;

    Ok(Response::new())
}

fn terminate(ctx: MutableCtx, user: Addr) -> anyhow::Result<Response> {
    let owner = ctx.querier.query_owner()?;

    ensure!(
        ctx.sender == owner,
        "you don't have the right, O you don't have the right"
    );

    let mut position = POSITIONS.load(ctx.storage, user)?;

    let vested = if let VestingStatus::Active(schedule) = &position.vesting_status {
        schedule.compute_claimable(ctx.block.timestamp, position.total)?
    } else {
        bail!("position is already terminated")
    };

    position.vesting_status = VestingStatus::Terminated(vested);

    // Any unvested tokens is clawed back.
    let refund = position.total.checked_sub(vested)?;
    let refund_msg = if refund.is_non_zero() {
        Some(Message::transfer(
            owner,
            Coin::new(dango::DENOM.clone(), refund)?,
        )?)
    } else {
        None
    };

    POSITIONS.save(ctx.storage, user, &position)?;

    Ok(Response::new().may_add_message(refund_msg))
}

fn claim(ctx: MutableCtx) -> anyhow::Result<Response> {
    let unlocking_schedule = UNLOCKING_SCHEDULE.load(ctx.storage)?;
    let mut position = POSITIONS.load(ctx.storage, ctx.sender)?;

    let claimable = position.compute_claimable(ctx.block.timestamp, &unlocking_schedule)?;

    ensure!(claimable.is_non_zero(), "nothing to claim");

    position.claimed.checked_add_assign(claimable)?;

    POSITIONS.save(ctx.storage, ctx.sender, &position)?;

    Ok(Response::new().add_message(Message::transfer(
        ctx.sender,
        Coin::new(dango::DENOM.clone(), claimable)?,
    )?))
}
