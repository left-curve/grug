use {
    crate::{
        force_write_on_query, infinite_loop, query_crypto_ed25519_batch_verify,
        query_crypto_recover_secp256k1, query_crypto_verify, query_force_write, query_loop,
        ExecuteMsg, InstantiateMsg, QueryMsg,
    },
    grug::{to_json_value, ImmutableCtx, Json, MutableCtx, Response, StdResult},
};

#[cfg_attr(not(feature = "library"), grug::export)]
pub fn instantiate(_ctx: MutableCtx, _msg: InstantiateMsg) -> StdResult<Response> {
    Ok(Response::new())
}

#[cfg_attr(not(feature = "library"), grug::export)]
pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> StdResult<Response> {
    match msg {
        ExecuteMsg::InfiniteLoop {} => infinite_loop(),
        ExecuteMsg::ForceWriteOnQuery { key, value } => force_write_on_query(ctx, key, value),
    }
}

#[cfg_attr(not(feature = "library"), grug::export)]
pub fn query(ctx: ImmutableCtx, msg: QueryMsg) -> StdResult<Json> {
    match msg {
        QueryMsg::Loop { iterations } => to_json_value(&query_loop(iterations)?),
        QueryMsg::ForceWrite { key, value } => to_json_value(&query_force_write(&key, &value)),
        QueryMsg::CryptoVerify {
            ty,
            pk,
            sig,
            msg_hash,
        } => to_json_value(&query_crypto_verify(ctx, ty, pk, sig, msg_hash)?),
        QueryMsg::RecoverSepc256k1 {
            sig,
            msg_hash,
            recovery_id,
            compressed,
        } => to_json_value(&query_crypto_recover_secp256k1(
            ctx,
            sig,
            msg_hash,
            recovery_id,
            compressed,
        )?),
        QueryMsg::Ed25519BatchVerify {
            prehash_msgs,
            sigs,
            pks,
        } => to_json_value(&query_crypto_ed25519_batch_verify(
            ctx,
            prehash_msgs,
            sigs,
            pks,
        )?),
    }
}
