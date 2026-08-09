pub mod core;
pub mod cron;
pub mod index_price;
pub mod maintain;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod position_index;
pub mod querier;
pub mod query;
pub mod referral;
pub mod state;
pub mod trade;
pub mod vault;

use {
    crate::state::{FEE_RATE_OVERRIDES, PAIR_PARAMS, PAIR_STATES, PARAM, STATE, USER_STATES},
    anyhow::{bail, ensure},
    dango_math::{NumberConst, Uint128},
    dango_order_book::{FillId, NEXT_FILL_ID, NEXT_ORDER_ID, OrderId, UsdValue},
    dango_primitives::{
        Addr, AuthCtx, AuthMode, Duration, EventBuilder, ImmutableCtx, Json, JsonDeExt, JsonSerExt,
        Message, MsgExecute, MutableCtx, Response, SudoCtx, Tx,
    },
    dango_types::{
        DangoQuerier,
        perps::{
            CancelConditionalOrderRequest, CancelOrderRequest, ExecuteMsg, InstantiateMsg,
            MaintainerMsg, QueryMsg, ReferralMsg, State, SubmitOrderRequest, TraderMsg, VaultMsg,
        },
    },
};

/// Virtual shares added to total supply in share price calculations.
/// Prevents the first-depositor attack (ERC-4626 inflation attack) by
/// ensuring the share price cannot be trivially inflated.
const VIRTUAL_SHARES: Uint128 = Uint128::new(1_000_000);

/// Virtual assets added to vault equity in share price calculations.
/// Works in tandem with `VIRTUAL_SHARES` to set the initial share price
/// and prevent share inflation attacks.
const VIRTUAL_ASSETS: UsdValue = UsdValue::new_int(1);

/// Lookback window for volume-tiered fee rate resolution.
const VOLUME_LOOKBACK: Duration = Duration::from_days(14);

/// Reject oracle prices for being too old if older than this threshold.
const MAX_ORACLE_STALENESS: Duration = Duration::from_millis(500);

/// Returns the oracle contract address.
///
/// In release builds, returns a compile-time constant for zero-cost lookups.
/// In debug/test builds, queries the chain's `AppConfig` at runtime so that
/// tests with dynamically-derived addresses work correctly.
#[inline]
fn oracle(querier: impl DangoQuerier) -> Addr {
    #[cfg(not(debug_assertions))]
    {
        let _ = querier;
        dango_primitives::addr!("cedc5f73cbb963a48471b849c3650e6e34cd3b6d")
    }

    #[cfg(debug_assertions)]
    {
        querier.query_oracle().unwrap()
    }
}

#[inline]
fn account_factory(querier: impl DangoQuerier) -> Addr {
    #[cfg(not(debug_assertions))]
    {
        let _ = querier;
        dango_primitives::addr!("18d28bafcdf9d4574f920ea004dea2d13ec16f6b")
    }

    #[cfg(debug_assertions)]
    {
        querier.query_account_factory().unwrap()
    }
}

pub fn instantiate(ctx: MutableCtx, msg: InstantiateMsg) -> anyhow::Result<Response> {
    STATE.save(
        ctx.storage,
        &State {
            last_funding_time: ctx.block.timestamp,
            ..Default::default()
        },
    )?;

    NEXT_ORDER_ID.save(ctx.storage, &OrderId::ONE)?;
    NEXT_FILL_ID.save(ctx.storage, &FillId::ONE)?;

    maintain::configure(ctx, msg.param, msg.pair_params)
}

pub fn cron_execute(ctx: SudoCtx) -> anyhow::Result<Response> {
    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    let mut events = EventBuilder::new();

    cron::process_unlocks(ctx.storage, ctx.block.timestamp, &mut events)?;

    cron::process_funding(ctx.storage, ctx.block.timestamp, ctx.contract)?;

    cron::process_conditional_orders(
        ctx.storage,
        ctx.querier,
        ctx.contract,
        ctx.block.timestamp,
        &mut events,
    )?;

    // Take the vault snapshot last, so equity reflects the end-of-block state
    // — including funding application and any conditional-order fills that
    // settled this block. Mirrors what the metrics path captures below.
    cron::take_vault_snapshot(ctx.storage, ctx.block.timestamp, ctx.contract)?;

    #[cfg(feature = "metrics")]
    {
        cron::emit_cron_metrics(ctx.storage, ctx.contract, start)?;
    }

    Ok(Response::new().add_events(events)?)
}

pub fn execute(ctx: MutableCtx, msg: ExecuteMsg) -> anyhow::Result<Response> {
    // Only `Deposit` and `Donate` methods accept attached funds (settlement currency).
    // Every other endpoint must be called without funds — tokens sent here would
    // otherwise be silently absorbed by the contract, lost to the sender.
    match msg {
        ExecuteMsg::Trade(TraderMsg::Deposit { .. })
        | ExecuteMsg::Maintain(MaintainerMsg::Donate {}) => {},
        _ => {
            ensure!(
                ctx.funds.is_empty(),
                "unexpected funds sent to non-deposit endpoint: {}",
                ctx.funds
            );
        },
    }

    match msg {
        ExecuteMsg::Maintain(msg) => match msg {
            MaintainerMsg::Configure { param, pair_params } => {
                maintain::configure(ctx, param, pair_params)
            },
            MaintainerMsg::Liquidate { user } => maintain::liquidate(ctx, user),
            MaintainerMsg::Donate {} => maintain::donate(ctx),
            MaintainerMsg::WithdrawTreasury {} => maintain::withdraw_treasury(ctx),
            MaintainerMsg::SetFeeRateOverride {
                user,
                maker_taker_fee_rates,
            } => maintain::set_fee_rate_override(ctx, user, maker_taker_fee_rates),
            MaintainerMsg::RefreshIndexPrices {} => maintain::refresh_index_prices(ctx),
            MaintainerMsg::RefreshVaultOrders {} => maintain::refresh_vault_orders(ctx),
        },
        ExecuteMsg::Trade(msg) => match msg {
            TraderMsg::Deposit { to } => trade::deposit(ctx, to),
            TraderMsg::Withdraw { amount } => trade::withdraw(ctx, amount),
            TraderMsg::SubmitOrder(SubmitOrderRequest {
                pair_id,
                size,
                kind,
                reduce_only,
                tp,
                sl,
            }) => trade::submit_order(ctx, pair_id, size, kind, reduce_only, tp, sl),
            TraderMsg::CancelOrder(CancelOrderRequest::One(order_id)) => {
                trade::cancel_one_order(ctx, order_id)
            },
            TraderMsg::CancelOrder(CancelOrderRequest::OneByClientOrderId(cid)) => {
                trade::cancel_one_order_by_client_order_id(ctx, cid)
            },
            TraderMsg::CancelOrder(CancelOrderRequest::All) => trade::cancel_all_orders(ctx),
            TraderMsg::BatchUpdateOrders(reqs) => trade::batch_update_orders(ctx, reqs),
            TraderMsg::SubmitConditionalOrder {
                pair_id,
                size,
                trigger_price,
                trigger_direction,
                max_slippage,
            } => trade::submit_conditional_order(
                ctx,
                pair_id,
                size,
                trigger_price,
                trigger_direction,
                max_slippage,
            ),
            TraderMsg::CancelConditionalOrder(CancelConditionalOrderRequest::One {
                pair_id,
                trigger_direction,
            }) => trade::cancel_one_conditional_order(ctx, pair_id, trigger_direction),
            TraderMsg::CancelConditionalOrder(CancelConditionalOrderRequest::AllForPair {
                pair_id,
            }) => trade::cancel_conditional_orders_for_pair(ctx, pair_id),
            TraderMsg::CancelConditionalOrder(CancelConditionalOrderRequest::All) => {
                trade::cancel_all_conditional_orders(ctx)
            },
        },
        ExecuteMsg::Vault(msg) => match msg {
            VaultMsg::AddLiquidity {
                amount,
                min_shares_to_mint,
            } => vault::add_liquidity(ctx, amount, min_shares_to_mint),
            VaultMsg::RemoveLiquidity { shares_to_burn } => {
                vault::remove_liquidity(ctx, shares_to_burn)
            },
        },
        ExecuteMsg::Referral(msg) => match msg {
            ReferralMsg::SetReferral { referrer, referee } => {
                referral::set_referral(ctx, referrer, referee)
            },
            ReferralMsg::SetFeeShareRatio { share_ratio } => {
                referral::set_fee_share_ratio(ctx, share_ratio)
            },
            ReferralMsg::SetCommissionRateOverride {
                user,
                commission_rate,
            } => referral::set_commission_rate_override(ctx, user, commission_rate),
            ReferralMsg::ForceSetFeeShareRatio { user, share_ratio } => {
                referral::force_set_fee_share_ratio(ctx, user, share_ratio)
            },
        },
    }
}

pub fn query(ctx: ImmutableCtx, msg: QueryMsg) -> anyhow::Result<Json> {
    match msg {
        QueryMsg::Param {} => {
            let res = PARAM.load(ctx.storage)?;
            res.to_json_value()
        },
        QueryMsg::PairParam { pair_id } => {
            let res = PAIR_PARAMS.may_load(ctx.storage, &pair_id)?;
            res.to_json_value()
        },
        QueryMsg::PairParams { start_after, limit } => {
            let res = query::query_pair_params(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::State {} => {
            let res = STATE.load(ctx.storage)?;
            res.to_json_value()
        },
        QueryMsg::PairState { pair_id } => {
            let res = PAIR_STATES.may_load(ctx.storage, &pair_id)?;
            res.to_json_value()
        },
        QueryMsg::PairStates { start_after, limit } => {
            let res = query::query_pair_states(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::UserState { user } => {
            let res = USER_STATES.may_load(ctx.storage, user)?;
            res.to_json_value()
        },
        QueryMsg::UserStates { start_after, limit } => {
            let res = query::query_user_states(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::UserStateExtended {
            user,
            include_equity,
            include_available_margin,
            include_maintenance_margin,
            include_unrealized_pnl,
            include_unrealized_funding,
            include_liquidation_price,
            include_all,
        } => {
            let res = query::query_user_state_extended(
                ctx.storage,
                user,
                include_equity,
                include_available_margin,
                include_maintenance_margin,
                include_unrealized_pnl,
                include_unrealized_funding,
                include_liquidation_price,
                include_all,
            )?;
            res.to_json_value()
        },
        QueryMsg::UserStatesExtended {
            start_after,
            limit,
            include_equity,
            include_available_margin,
            include_maintenance_margin,
            include_unrealized_pnl,
            include_unrealized_funding,
            include_liquidation_price,
            include_all,
        } => {
            let res = query::query_user_states_extended(
                ctx.storage,
                start_after,
                limit,
                include_equity,
                include_available_margin,
                include_maintenance_margin,
                include_unrealized_pnl,
                include_unrealized_funding,
                include_liquidation_price,
                include_all,
            )?;
            res.to_json_value()
        },
        QueryMsg::Order { order_id } => {
            let res = query::query_order(ctx, order_id)?;
            res.to_json_value()
        },
        QueryMsg::OrdersByUser { user } => {
            let res = query::query_orders_by_user(ctx, user)?;
            res.to_json_value()
        },
        QueryMsg::OrderByClientOrderId {
            user,
            client_order_id,
        } => {
            let res = query::query_order_by_client_order_id(ctx, user, client_order_id)?;
            res.to_json_value()
        },
        QueryMsg::LiquidityDepth {
            pair_id,
            bucket_size,
            limit,
        } => {
            let res = query::query_liquidity_depth(ctx, pair_id, bucket_size, limit)?;
            res.to_json_value()
        },
        QueryMsg::Volume { user, since } => {
            let res = query::query_volume(ctx.storage, user, since)?;
            res.to_json_value()
        },
        QueryMsg::VolumeByUser { user, since } => {
            let res = query::query_volume_by_user(ctx, user, since)?;
            res.to_json_value()
        },
        QueryMsg::VaultSnapshots { min, max } => {
            let res = query::query_vault_snapshots(ctx.storage, min, max)?;
            res.to_json_value()
        },
        QueryMsg::Referrer { referee } => {
            let res = query::query_referrer(ctx.storage, referee)?;
            res.to_json_value()
        },
        QueryMsg::ReferralData { user, since } => {
            let res = query::query_referral_data(ctx, user, since)?;
            res.to_json_value()
        },
        QueryMsg::ReferralDataEntries {
            user,
            start_after,
            limit,
        } => {
            let res = query::query_referral_data_entries(ctx, user, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::ReferrerToRefereeStats { referrer, order_by } => {
            let res = query::query_referrer_to_referee_stats(ctx, referrer, order_by)?;
            res.to_json_value()
        },
        QueryMsg::ReferralSettings { user } => {
            let res = query::query_referral_settings(ctx, user)?;
            res.to_json_value()
        },
        QueryMsg::CommissionRateOverride { user } => {
            let res = query::query_commission_rate_override(ctx.storage, user)?;
            res.to_json_value()
        },
        QueryMsg::CommissionRateOverrides { start_after, limit } => {
            let res = query::query_commission_rate_overrides(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::FeeRateOverride { user } => {
            let res = FEE_RATE_OVERRIDES.may_load(ctx.storage, user)?;
            res.to_json_value()
        },
        QueryMsg::FeeRateOverrides { start_after, limit } => {
            let res = query::query_fee_rate_overrides(ctx, start_after, limit)?;
            res.to_json_value()
        },
    }
    .map_err(Into::into)
}

pub fn authenticate(ctx: AuthCtx, tx: Tx) -> anyhow::Result<Response> {
    ensure!(
        ctx.mode == AuthMode::Finalize,
        "you don't have the right, O you don't have the right"
    );

    for msg in tx.msgs.iter() {
        let Message::Execute(MsgExecute { contract, msg, .. }) = msg else {
            bail!("all messages must be execute messages");
        };

        ensure!(
            contract == ctx.contract,
            "all messages must target the perps contract"
        );

        let Ok(ExecuteMsg::Maintain(
            MaintainerMsg::RefreshIndexPrices {} | MaintainerMsg::RefreshVaultOrders {},
        )) = msg.clone().deserialize_json()
        else {
            bail!("all execute messages must be RefreshIndexPrices or RefreshVaultOrders");
        };
    }

    Ok(Response::new())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{Coins, Json, MockContext, NonEmpty, ResultExt},
        std::collections::BTreeMap,
    };

    const CONTRACT: Addr = Addr::mock(0);

    fn make_tx(msgs: Vec<Message>) -> Tx {
        Tx {
            sender: CONTRACT,
            gas_limit: 1_000_000,
            msgs: NonEmpty::new_unchecked(msgs),
            data: Json::null(),
            credential: Json::null(),
        }
    }

    fn refresh_index_prices_msg(contract: Addr) -> Message {
        Message::execute(
            contract,
            &ExecuteMsg::Maintain(MaintainerMsg::RefreshIndexPrices {}),
            Coins::new(),
        )
        .unwrap()
    }

    fn refresh_vault_orders_msg(contract: Addr) -> Message {
        Message::execute(
            contract,
            &ExecuteMsg::Maintain(MaintainerMsg::RefreshVaultOrders {}),
            Coins::new(),
        )
        .unwrap()
    }

    #[test]
    fn authenticate_single_refresh_index_prices() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Finalize);

        let tx = make_tx(vec![refresh_index_prices_msg(CONTRACT)]);
        authenticate(ctx.as_auth(), tx).should_succeed();
    }

    #[test]
    fn authenticate_single_refresh_vault_orders() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Finalize);

        let tx = make_tx(vec![refresh_vault_orders_msg(CONTRACT)]);
        authenticate(ctx.as_auth(), tx).should_succeed();
    }

    #[test]
    fn authenticate_both_messages() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Finalize);

        let tx = make_tx(vec![
            refresh_index_prices_msg(CONTRACT),
            refresh_vault_orders_msg(CONTRACT),
        ]);
        authenticate(ctx.as_auth(), tx).should_succeed();
    }

    #[test]
    fn authenticate_rejects_check_mode() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Check);

        let tx = make_tx(vec![refresh_index_prices_msg(CONTRACT)]);
        authenticate(ctx.as_auth(), tx).should_fail_with_error("you don't have the right");
    }

    #[test]
    fn authenticate_rejects_wrong_contract() {
        let wrong_contract = Addr::mock(99);
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Finalize);

        let tx = make_tx(vec![refresh_index_prices_msg(wrong_contract)]);
        authenticate(ctx.as_auth(), tx)
            .should_fail_with_error("all messages must target the perps contract");
    }

    #[test]
    fn authenticate_rejects_non_execute_message() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Finalize);

        let tx = make_tx(vec![Message::Transfer(BTreeMap::new())]);
        authenticate(ctx.as_auth(), tx)
            .should_fail_with_error("all messages must be execute messages");
    }

    #[test]
    fn authenticate_rejects_invalid_execute() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Finalize);

        let msg = Message::execute(
            CONTRACT,
            &ExecuteMsg::Maintain(MaintainerMsg::Donate {}),
            Coins::new(),
        )
        .unwrap();

        let tx = make_tx(vec![msg]);
        authenticate(ctx.as_auth(), tx).should_fail_with_error(
            "all execute messages must be RefreshIndexPrices or RefreshVaultOrders",
        );
    }

    #[test]
    fn authenticate_rejects_mixed_valid_invalid() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_mode(AuthMode::Finalize);

        let invalid_msg = Message::execute(
            CONTRACT,
            &ExecuteMsg::Maintain(MaintainerMsg::Donate {}),
            Coins::new(),
        )
        .unwrap();

        let tx = make_tx(vec![refresh_index_prices_msg(CONTRACT), invalid_msg]);
        authenticate(ctx.as_auth(), tx).should_fail_with_error(
            "all execute messages must be RefreshIndexPrices or RefreshVaultOrders",
        );
    }
}
