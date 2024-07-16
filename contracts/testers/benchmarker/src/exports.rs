use crate::{execute::populate, read_data, ExecuteMsg};
#[cfg(not(feature = "library"))]
use grug::grug_export;

use {
    crate::{do_loop, QueryMsg},
    grug::{to_json_value, Empty, ImmutableCtx, Json, MutableCtx, Response, StdResult},
};

#[cfg_attr(not(feature = "library"), grug_export)]
pub fn instantiate(_ctx: MutableCtx, _msg: Empty) -> StdResult<Response> {
    Ok(Response::new())
}

#[cfg_attr(not(feature = "library"), grug_export)]
pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> StdResult<Response> {
    ctx.api.debug(&ctx.contract, "init execute");
    match msg {
        ExecuteMsg::Populate { data } => populate(ctx, data),
    }
}

#[cfg_attr(not(feature = "library"), grug_export)]
pub fn query(ctx: ImmutableCtx, msg: QueryMsg) -> StdResult<Json> {
    match msg {
        QueryMsg::Loop { iterations } => to_json_value(&do_loop(iterations)?),
        QueryMsg::Data {
            min,
            max,
            order,
            limit,
            sized,
        } => to_json_value(&read_data(ctx.storage, min, max, order, limit, sized)?),
    }
}
