use {
    crate::{
        state::PARAM,
        trade::{
            _cancel_all_orders, _cancel_one_order, _cancel_one_order_by_client_order_id,
            _submit_order, ensure_trading_enabled,
        },
    },
    anyhow::ensure,
    dango_primitives::{EventBuilder, Inner, MutableCtx, NonEmpty, Response},
    dango_types::perps::{CancelOrderRequest, SubmitOrCancelOrderRequest, SubmitOrderRequest},
};

/// Execute a sequence of submit / cancel actions atomically.
///
/// Each action is applied sequentially to a single shared `&mut dyn
/// Storage`; later actions observe the state written by earlier ones
/// (the engine's `Buffer` makes in-call writes visible to subsequent reads).
/// If any action returns `Err`, the `?` propagation bubbles the error
/// out of `execute`, the engine drops the buffer without flushing, and every
/// prior write in this batch is discarded — no partial state persists.
///
/// Events accumulate into a single `EventBuilder` in action order and
/// are emitted together on the returned `Response`.
pub fn batch_update_orders(
    ctx: MutableCtx,
    reqs: NonEmpty<Vec<SubmitOrCancelOrderRequest>>,
) -> anyhow::Result<Response> {
    let param = PARAM.load(ctx.storage)?;

    // Gate the whole batch rather than only its submit actions: a batch that
    // mixes cancels with submits must fail as a unit, not partially apply.
    ensure_trading_enabled(&param)?;

    // Enforce the governance-tunable upper bound on batch size.
    ensure!(
        reqs.len() <= param.max_action_batch_size,
        "invalid batch size! bounds: <= `max_action_batch_size` ({}), found: {}",
        param.max_action_batch_size,
        reqs.len(),
    );

    let mut events = EventBuilder::new();

    for req in reqs.into_inner() {
        match req {
            SubmitOrCancelOrderRequest::Submit(SubmitOrderRequest {
                pair_id,
                size,
                kind,
                reduce_only,
                tp,
                sl,
            }) => _submit_order(
                ctx.storage,
                ctx.querier,
                ctx.block.timestamp,
                ctx.contract,
                ctx.sender,
                pair_id,
                size,
                kind,
                reduce_only,
                tp,
                sl,
                &mut events,
            )?,
            SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::One(order_id)) => {
                _cancel_one_order(ctx.storage, ctx.sender, order_id, &mut events)?
            },
            SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::OneByClientOrderId(cid)) => {
                _cancel_one_order_by_client_order_id(ctx.storage, ctx.sender, cid, &mut events)?
            },
            SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::All) => {
                _cancel_all_orders(ctx.storage, ctx.sender, &mut events)?
            },
        }
    }

    Ok(Response::new().add_events(events)?)
}
