use {
    anyhow::ensure,
    dango_math::IsZero,
    dango_primitives::{AuthCtx, Coins, Denom, Message, MutableCtx, QuerierExt, Response, Tx},
    dango_types::{
        DangoQuerier,
        account::{ExecuteMsg, InstantiateMsg},
        gateway::{self, Addr32, Remote},
    },
};

pub fn instantiate(ctx: MutableCtx, msg: InstantiateMsg) -> anyhow::Result<Response> {
    dango_auth::create_account(ctx, msg.activate)?;

    Ok(Response::new())
}

pub fn authenticate(ctx: AuthCtx, tx: Tx) -> anyhow::Result<Response> {
    dango_auth::authenticate_tx(ctx, tx, None)?;

    Ok(Response::new())
}

pub fn receive(ctx: MutableCtx) -> anyhow::Result<Response> {
    dango_auth::receive_transfer(ctx)?;

    Ok(Response::new())
}

pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> anyhow::Result<Response> {
    match msg {
        ExecuteMsg::ForceWithdrawal {
            denom,
            remote,
            recipient,
        } => force_withdrawal(ctx, denom, remote, recipient),
    }
}

fn force_withdrawal(
    ctx: MutableCtx,
    denom: Denom,
    remote: Remote,
    recipient: Addr32,
) -> anyhow::Result<Response> {
    ensure!(
        ctx.sender == ctx.querier.query_owner()?,
        "you don't have the right, O you don't have the right"
    );

    // The withdrawal is funded from the account's own balance, so any coins the
    // owner attaches would be stranded here.
    ensure!(
        ctx.funds.is_empty(),
        "don't send funds when forcing a withdrawal"
    );

    let amount = ctx.querier.query_balance(ctx.contract, denom.clone())?;

    ensure!(
        amount.is_non_zero(),
        "account {} holds no {denom}",
        ctx.contract
    );

    let gateway = ctx.querier.query_gateway()?;

    Ok(Response::new().add_message(Message::execute(
        gateway,
        &gateway::ExecuteMsg::TransferRemote { remote, recipient },
        Coins::one(denom, amount)?,
    )?))
}
