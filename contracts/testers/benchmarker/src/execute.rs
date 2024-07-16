use grug::{MutableCtx, Response, StdResult, Uint128};

use crate::state::DATA;

pub fn populate(ctx: MutableCtx, data: Vec<(String, Uint128)>) -> StdResult<Response> {
    for (k, v) in data {
        DATA.save(ctx.storage, &k, &v).unwrap();
    }

    Ok(Response::new())
}
