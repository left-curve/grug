use {
    crate::{
        core::{
            compute_bankruptcy_price, compute_close_schedule, compute_maintenance_margin,
            compute_user_equity, compute_user_equity_with_pnl, is_liquidatable,
        },
        position_index::{
            PositionIndexUpdate, apply_position_index_updates, compute_position_diff,
        },
        querier::NoCachePerpQuerier,
        state::{LONGS, PAIR_PARAMS, PAIR_STATES, PARAM, SHORTS, STATE, USER_STATES},
        trade::{
            CancelAllOrdersOutcome, FeeBreakdown, MatchOrderOutcome,
            compute_cancel_all_orders_outcome, match_order, merge_fee_breakdown,
            resize_reduce_only_orders, settle_fill, settle_pnls,
        },
    },
    anyhow::ensure,
    dango_math::NumberConst,
    dango_order_book::{
        ASKS, BIDS, ConditionalOrderRemoved, Dimensionless, FillId, LimitOrder, NEXT_FILL_ID,
        NEXT_ORDER_ID, OrderId, PairId, Quantity, ReasonForOrderRemoval, TriggerDirection,
        UsdPrice, UsdValue, decrease_liquidity_depths, flush_volumes, increase_liquidity_depths,
        may_invert_price,
    },
    dango_primitives::{
        Addr, EventBuilder, MutableCtx, Order as IterationOrder, Response, Storage, Timestamp,
    },
    dango_types::perps::{
        BadDebtCovered, Deleveraged, Liquidated, PairParam, PairState, Param, RateSchedule, State,
        UserState,
    },
    std::collections::BTreeMap,
};

/// Liquidate an underwater trader by closing their positions.
///
/// Unfilled positions are ADL'd against counter-parties at the bankruptcy price.
/// Any remaining bad debt is absorbed by the insurance fund.
///
/// Mutates: `STATE`, `PAIR_STATES`, `USER_STATES` (liquidated user + makers +
/// ADL counter-parties), `LONGS`, `SHORTS`.
///
/// Returns: empty `Response` (all PnL/fees settled via internal margins).
pub fn liquidate(ctx: MutableCtx, user: Addr) -> anyhow::Result<Response> {
    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    // --------------------- 1. Preparation + basic checks ---------------------

    let param = PARAM.load(ctx.storage)?;
    let state = STATE.load(ctx.storage)?;

    let user_state = USER_STATES.may_load(ctx.storage, user)?.unwrap_or_default();

    let mut events = EventBuilder::new();

    // -------------------- 2. Cancel all resting orders -----------------------

    let CancelAllOrdersOutcome { mut user_state } = compute_cancel_all_orders_outcome(
        ctx.storage,
        user,
        &user_state,
        Some(&mut events),
        ReasonForOrderRemoval::Liquidated,
    )?;

    // Cancel all embedded conditional orders. Positions may survive partial
    // liquidation, so we must explicitly clear the fields.
    for (pair_id, position) in &mut user_state.positions {
        if position.conditional_order_above.take().is_some() {
            events.push(ConditionalOrderRemoved {
                pair_id: pair_id.clone(),
                user,
                trigger_direction: TriggerDirection::Above,
                reason: ReasonForOrderRemoval::Liquidated,
            })?;
        }
        if position.conditional_order_below.take().is_some() {
            events.push(ConditionalOrderRemoved {
                pair_id: pair_id.clone(),
                user,
                trigger_direction: TriggerDirection::Below,
                reason: ReasonForOrderRemoval::Liquidated,
            })?;
        }
    }

    // ------------------- 3. Load pair params and states ---------------------

    // Collect pair IDs first to avoid borrow conflicts.
    let pair_ids = user_state.positions.keys().cloned().collect::<Vec<_>>();

    let mut pair_params = BTreeMap::new();
    let mut pair_states = BTreeMap::new();

    for pair_id in &pair_ids {
        let pair_param = PAIR_PARAMS.load(ctx.storage, pair_id)?;
        let pair_state = PAIR_STATES.load(ctx.storage, pair_id)?;

        pair_params.insert(pair_id.clone(), pair_param);
        pair_states.insert(pair_id.clone(), pair_state);
    }

    // -------------------- 4. Compute oracle prices ---------------------------

    let oracle_prices = pair_states
        .iter()
        .map(|(pair_id, pair_state)| (pair_id.clone(), pair_state.index_price))
        .collect::<BTreeMap<_, _>>();

    // --------------------------- 5. Business logic ---------------------------

    let LiquidateOutcome {
        state,
        pair_states,
        user_state,
        maker_states,
        order_mutations,
        index_updates,
        volumes,
        next_order_id,
        next_fill_id,
    } = _liquidate(
        ctx.storage,
        user,
        ctx.contract,
        ctx.block.timestamp,
        &param,
        &state,
        &pair_params,
        &pair_states,
        &user_state,
        &oracle_prices,
        &mut events,
    )?;

    // --------------------- 6. Apply state changes ----------------------------

    flush_volumes(ctx.storage, ctx.block.timestamp, &volumes)?;

    STATE.save(ctx.storage, &state)?;

    NEXT_ORDER_ID.save(ctx.storage, &next_order_id)?;
    NEXT_FILL_ID.save(ctx.storage, &next_fill_id)?;

    for (pair_id, pair_state) in &pair_states {
        PAIR_STATES.save(ctx.storage, pair_id, pair_state)?;
    }

    if user_state.is_empty() {
        USER_STATES.remove(ctx.storage, user)?;
    } else {
        USER_STATES.save(ctx.storage, user, &user_state)?;
    }

    for (addr, maker_state) in &maker_states {
        if maker_state.is_empty() {
            USER_STATES.remove(ctx.storage, *addr)?;
        } else {
            USER_STATES.save(ctx.storage, *addr, maker_state)?;
        }
    }

    // -------------------- 7. Apply order mutations ---------------------------

    for (pair_id, taker_is_bid, stored_price, order_id, mutation, pre_fill_abs_size) in
        order_mutations
    {
        let order_key = (pair_id.clone(), stored_price, order_id);

        let maker_book = if taker_is_bid {
            ASKS
        } else {
            BIDS
        };

        // The maker is on the opposite side of the taker.
        let maker_is_bid = !taker_is_bid;
        let real_price = may_invert_price(stored_price, maker_is_bid);

        let pair_param = pair_params.get(&pair_id).unwrap();

        // Complete remove the order's liquidity depth contribution, and re-add
        // the remaining size (if any) to prevent notional drift.
        decrease_liquidity_depths(
            ctx.storage,
            &pair_id,
            maker_is_bid,
            real_price,
            pre_fill_abs_size,
            &pair_param.bucket_sizes,
        )?;

        match mutation {
            Some(order) => {
                increase_liquidity_depths(
                    ctx.storage,
                    &pair_id,
                    maker_is_bid,
                    real_price,
                    order.size.checked_abs()?,
                    &pair_param.bucket_sizes,
                )?;

                maker_book.save(ctx.storage, order_key, &order)?;
            },
            None => {
                maker_book.remove(ctx.storage, order_key)?;
            },
        }
    }

    // -------------------- 8. Apply position index updates --------------------

    apply_position_index_updates(ctx.storage, &index_updates)?;

    // ---------- 9. Dynamic re-size of reduce-only orders ---------------------
    //
    // Liquidation forcibly reduced the positions of book-fill makers and ADL
    // counter-parties (the liquidated user's own orders were already cancelled
    // in step 2), so each of their resting reduce-only orders is re-clamped to
    // its new position. The affected users are the keys of `maker_states`; the
    // pairs are those the liquidated user held (`pair_ids`), the only pairs a
    // fill could have touched. Over-scanning (a maker not on a given pair) is a
    // safe no-op. We iterate the maps rather than the `index_updates`, because a
    // pure ADL reduction leaves `(entry_price, side)` — hence the index —
    // unchanged, which is exactly the case re-sizing must catch.
    for pair_id in &pair_ids {
        for maker in maker_states.keys() {
            resize_reduce_only_orders(ctx.storage, *maker, pair_id, &mut events)?;
        }
    }

    #[cfg(feature = "tracing")]
    {
        tracing::info!(
            %user,
            num_positions = pair_ids.len(),
            "Liquidation executed"
        );
    }

    #[cfg(feature = "metrics")]
    {
        metrics::counter!(crate::metrics::LABEL_LIQUIDATIONS).increment(1);

        metrics::histogram!(crate::metrics::LABEL_DURATION_LIQUIDATE)
            .record(start.elapsed().as_secs_f64());

        // OI gauges are updated per pair after liquidation.
        for (pair_id, pair_state) in &pair_states {
            let pair_label = pair_id.to_string();

            metrics::gauge!(
                crate::metrics::LABEL_OPEN_INTEREST_LONG,
                "pair_id" => pair_label.clone()
            )
            .set(pair_state.long_oi.to_f64());

            metrics::gauge!(
                crate::metrics::LABEL_OPEN_INTEREST_SHORT,
                "pair_id" => pair_label
            )
            .set(pair_state.short_oi.to_f64());
        }
    }

    Ok(Response::new().add_events(events)?)
}

/// Owned outcome of a `_liquidate` call. Returns every piece of
/// caller-persistable state the liquidation may have updated, so failure
/// leaves no trace in the caller's locals. The `order_mutations` list is
/// tagged with `(pair_id, taker_is_bid, ...)` because a liquidation can
/// close positions across multiple pairs in a single call.
#[derive(Debug)]
pub struct LiquidateOutcome {
    pub state: State,
    pub pair_states: BTreeMap<PairId, PairState>,
    pub user_state: UserState,
    pub maker_states: BTreeMap<Addr, UserState>,
    pub order_mutations: Vec<(
        PairId,
        bool,
        UsdPrice,
        OrderId,
        Option<LimitOrder>,
        Quantity,
    )>,
    pub index_updates: Vec<PositionIndexUpdate>,
    pub volumes: BTreeMap<Addr, UsdValue>,
    pub next_order_id: OrderId,
    pub next_fill_id: FillId,
}

/// Pure liquidation core: takes the dense state structs by `&` and
/// returns the updated copies in [`LiquidateOutcome`]. The body clones
/// `state`, `pair_states`, and `user_state` at entry and mutates the
/// locals. Inner helpers `execute_close_schedule`, `execute_adl`,
/// `settle_pnls`, and `settle_fill` continue to take `&mut` parameters
/// — they are leaf helpers operating on `_liquidate`'s own local
/// buffers, so the pragmatic exception in `dango/exchange/perps/purity.md`
/// applies and the bug class is still impossible.
fn _liquidate(
    storage: &dyn Storage,
    user: Addr,
    contract: Addr,
    current_time: Timestamp,
    param: &Param,
    state: &State,
    pair_params: &BTreeMap<PairId, PairParam>,
    pair_states: &BTreeMap<PairId, PairState>,
    user_state: &UserState,
    oracle_prices: &BTreeMap<PairId, UsdPrice>,
    events: &mut EventBuilder,
) -> anyhow::Result<LiquidateOutcome> {
    // Clone at entry and mutate locals freely. `events` is the one
    // deliberate `&mut` on caller state per the purity rule exception.
    let mut state = state.clone();
    let mut pair_states = pair_states.clone();
    let mut user_state = user_state.clone();

    // -------------------- Step 1: Assert liquidatable ------------------------

    let perp_querier = NoCachePerpQuerier::new_local(storage);

    let (is_liquidatable, equity, maintenance_margin) =
        is_liquidatable(&perp_querier, &user_state)?;

    ensure!(
        is_liquidatable,
        "user is not liquidatable! equity = {equity}, maintenance margin = {maintenance_margin}"
    );

    // ------------- Step 2: Compute close schedule (largest-MM-first) ---------

    // Compute the deficit. This is the shortfall between the user's equity and
    // maintenance margin (MM) + a buffer.
    // We need to close positions such that MM + buffer <= equity.
    let deficit = {
        let equity = compute_user_equity(&perp_querier, &user_state)?;
        let mm = compute_maintenance_margin(&perp_querier, &user_state)?;
        let one_plus_buffer = Dimensionless::ONE.checked_add(param.liquidation_buffer_ratio)?;
        let effective_equity = equity.checked_div(one_plus_buffer)?;
        mm.checked_sub(effective_equity)?
    };

    // Compute which positions to close and how much to close based on the deficit.
    let schedule = compute_close_schedule(&user_state, pair_params, oracle_prices, deficit)?;

    // `compute_close_schedule` is supposed to produce at least one entry
    // whenever `deficit > 0`, which is implied by `is_liquidatable` passing above.
    ensure!(
        !schedule.is_empty(),
        "close schedule is empty despite `is_liquidatable` passing — invariant violated"
    );

    // -------- Step 3: Execute closes via the order book + ADL ----------------

    let mut all_maker_states = BTreeMap::new();

    let (
        updated_state,
        _all_fee_breakdowns,
        all_order_mutations,
        closed_notional,
        all_index_updates,
        all_volumes,
        next_order_id,
        next_fill_id,
    ) = execute_close_schedule(
        storage,
        user,
        contract,
        current_time,
        param,
        state,
        pair_params,
        &mut pair_states,
        &mut user_state,
        &mut all_maker_states,
        &schedule,
        oracle_prices,
        events,
    )?;

    state = updated_state;

    // -------------------- Step 4: Liquidation fee → insurance fund -----------

    // Per-fill settlement inside `match_order` + `execute_adl` has already
    // applied realized PnLs to `user_state.margin`. Trading fees during
    // liquidation are zero (liq_param zeroes fee rates), so the liquidation
    // fee is the only additional margin hit — routed straight to the
    // insurance fund, bypassing the protocol/vault fee split.
    //
    // The fee is capped at the user's *equity* (margin + unrealized PnL of any
    // remaining positions), not margin alone, so it can never drive equity
    // below zero — i.e. the fee itself never creates bad debt. Realized PnL is
    // already settled into `user_state.margin`, so the accumulators passed to
    // the equity helper are zero, matching the bankruptcy-price convention
    // used in `execute_close_schedule`.
    let user_equity =
        compute_user_equity_with_pnl(&user_state, oracle_prices, UsdValue::ZERO, UsdValue::ZERO)?;

    let liq_fee =
        compute_liquidation_fee(closed_notional, param.liquidation_fee_rate, user_equity)?;

    if liq_fee.is_non_zero() {
        user_state.margin.checked_sub_assign(liq_fee)?;
        state.insurance_fund.checked_add_assign(liq_fee)?;
    }

    // -------------------- Step 5: Bad debt → insurance fund ------------------

    // Bad debt is a *negative-equity* condition, not merely a negative margin
    // balance. A cross-margined account can carry negative margin while
    // remaining solvent when its remaining positions hold unrealized profit
    // (the user withdrew against, or realized a loss against, that profit before
    // liquidation). Recognizing bad debt off the margin sign in that case would
    // gift insurance-fund value to a solvent user.
    //
    // The fee is the only thing that changed the account since `user_equity` was
    // computed above — it just lowered `margin`, leaving positions untouched —
    // so the post-fee equity is exactly `user_equity - liq_fee`. No need to
    // recompute the per-position sum.
    let user_equity = user_equity.checked_sub(liq_fee)?;

    if user_equity.is_negative() {
        let bad_debt = user_equity.checked_abs()?;

        // Top the account up to exactly zero equity by crediting its margin,
        // and deduct the same amount from the insurance fund (which can go
        // negative as a last resort). When the account is fully closed (the
        // normal insolvent path) equity == margin, so this is identical to
        // flooring margin to zero.
        user_state.margin.checked_add_assign(bad_debt)?;
        state.insurance_fund.checked_sub_assign(bad_debt)?;

        events.push(BadDebtCovered {
            liquidated_user: user,
            amount: bad_debt,
            insurance_fund_remaining: state.insurance_fund,
        })?;

        #[cfg(feature = "tracing")]
        {
            tracing::warn!(
                %user,
                %bad_debt,
                insurance_fund_remaining = %state.insurance_fund,
                "!!! BAD DEBT COVERED !!!"
            );
        }

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(crate::metrics::LABEL_BAD_DEBT).record(bad_debt.to_f64().abs());
        }
    }

    Ok(LiquidateOutcome {
        state,
        pair_states,
        user_state,
        maker_states: all_maker_states,
        order_mutations: all_order_mutations,
        index_updates: all_index_updates,
        volumes: all_volumes,
        next_order_id,
        next_fill_id,
    })
}

/// Execute the close schedule against the order book, with ADL for any unfilled
/// remainder.
///
/// `maker_states` is a shared map of maker `UserState`s that persists across
/// `match_order` calls.
///
/// This is a leaf helper private to `_liquidate` and keeps `&mut` parameters
/// by design; it is not part of the pure set.
fn execute_close_schedule(
    storage: &dyn Storage,
    user: Addr,
    contract: Addr,
    current_time: Timestamp,
    param: &Param,
    state: State,
    pair_params: &BTreeMap<PairId, PairParam>,
    pair_states: &mut BTreeMap<PairId, PairState>,
    user_state: &mut UserState,
    maker_states: &mut BTreeMap<Addr, UserState>,
    schedule: &[(PairId, Quantity)],
    oracle_prices: &BTreeMap<PairId, UsdPrice>,
    events: &mut EventBuilder,
) -> anyhow::Result<(
    State,
    BTreeMap<Addr, FeeBreakdown>,
    Vec<(
        PairId,
        bool,
        UsdPrice,
        OrderId,
        Option<LimitOrder>,
        Quantity,
    )>,
    UsdValue,
    Vec<PositionIndexUpdate>,
    BTreeMap<Addr, UsdValue>,
    OrderId,
    FillId,
)> {
    let mut state = state;
    let mut next_order_id = NEXT_ORDER_ID.load(storage)?;
    let mut next_fill_id = NEXT_FILL_ID.load(storage)?;

    // Zero-fee param for liquidation fills.
    let liq_param = Param {
        maker_fee_rates: RateSchedule::default(),
        taker_fee_rates: RateSchedule::default(),
        ..param.clone()
    };

    let mut all_fee_breakdowns = BTreeMap::<Addr, FeeBreakdown>::new();
    let mut all_volumes = BTreeMap::<_, UsdValue>::new();
    let mut all_order_mutations = Vec::new();
    let mut closed_notional = UsdValue::ZERO;
    let mut all_index_updates = Vec::new();

    for (pair_id, close_size) in schedule {
        let pair_state = pair_states.get_mut(pair_id).unwrap();
        let oracle_price = oracle_prices[pair_id];

        let taker_is_bid = close_size.is_positive();

        // Compute the bankruptcy price BEFORE book fills. It divides the
        // user's total equity by the position's FULL size — not the scheduled
        // close amount — so a partial close at this price concedes only the
        // closed fraction's share of equity (`equity / |size|` per unit),
        // never more. Dividing by a deficit-sized partial close instead would
        // amplify the offset and let a single small fill confiscate the whole
        // account's equity, which is the bug behind the testnet block
        // 32500262 liquidation cascade.
        //
        // The price serves two roles:
        //
        // 1. When equity > 0 (timely liquidation): bp sits within the
        //    maintenance-margin band below oracle (above for shorts), and is
        //    used as both the target_price for book matching AND the ADL fill
        //    price. This prevents matching against absurd resting orders and
        //    guarantees equity_after >= 0: closing `c` of `|size|` units
        //    leaves equity × (1 - c/|size|).
        //
        // 2. When equity <= 0 (late liquidation): bp overshoots oracle (above
        //    oracle for longs, below for shorts), which would block valid
        //    oracle-adjacent book fills. In this case we fall back to the
        //    oracle price as target_price and use bp only for ADL. The
        //    schedule fully closes the position in this case (deficit >= MM),
        //    so a pure-ADL close at bp zeroes equity exactly — no bad debt.
        //
        // Per-fill settlement inside `match_order` has already applied
        // realized PnLs and fees (zero during liquidation) from earlier
        // pairs to `user_state.margin`, so the "pre" PnL / fee deltas
        // passed to equity helpers are zero — user_state is already up
        // to date.
        let bankruptcy_price = compute_bankruptcy_price(
            user_state,
            pair_id,
            oracle_prices,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )?;

        let equity = compute_user_equity_with_pnl(
            user_state,
            oracle_prices,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )?;

        let target_price = if equity.is_positive() {
            bankruptcy_price
        } else {
            oracle_price
        };

        // `match_order` settles fees and PnLs on margins per-fill; destructure
        // its outcome and write the dense-state fields back through the
        // caller's `&mut`s so the rest of `execute_close_schedule`'s body
        // keeps working. The liquidated user IS the taker, so
        // `updated_taker_state` goes into `*user_state`.
        let MatchOrderOutcome {
            state: updated_state,
            pair_state: updated_pair_state,
            taker_state: updated_user_state,
            maker_states: updated_maker_states,
            unfilled,
            volumes,
            fee_breakdowns,
            order_mutations,
            index_updates,
            next_order_id: updated_next_order_id,
            next_fill_id: updated_next_fill_id,
        } = match_order(
            storage,
            user,
            contract,
            current_time,
            &liq_param,
            &state,
            pair_id,
            pair_state,
            user_state,
            taker_is_bid,
            OrderId::ZERO,
            None,                      // liquidation has no client_order_id
            Dimensionless::ZERO,       // zero trading fee for the liquidated taker
            Some(Dimensionless::ZERO), // zero trading fee for makers, even if they have overrides
            maker_states,
            target_price,
            oracle_price,
            pair_params.get(pair_id).unwrap().max_limit_price_deviation,
            *close_size,
            next_order_id,
            next_fill_id,
            events,
        )?;

        state = updated_state;
        *pair_state = updated_pair_state;
        *user_state = updated_user_state;
        *maker_states = updated_maker_states;
        next_order_id = updated_next_order_id;
        next_fill_id = updated_next_fill_id;

        // Merge per-party fee breakdowns across pairs (empty during
        // liquidation, but kept for structural symmetry with `submit_order`).
        for (addr, bd) in fee_breakdowns {
            merge_fee_breakdown(&mut all_fee_breakdowns, addr, bd)?;
        }

        // Merge volumes.
        for (addr, vol) in volumes {
            all_volumes
                .entry(addr)
                .or_default()
                .checked_add_assign(vol)?;
        }

        // Collect order mutations with pair context.
        for (stored_price, order_id, mutation, pre_fill_abs_size) in order_mutations {
            all_order_mutations.push((
                pair_id.clone(),
                taker_is_bid,
                stored_price,
                order_id,
                mutation,
                pre_fill_abs_size,
            ));
        }

        all_index_updates.extend(index_updates);

        // Track closed notional for fee calculation.
        let filled = close_size.checked_sub(unfilled)?;
        closed_notional.checked_add_assign(filled.checked_abs()?.checked_mul(oracle_price)?)?;

        // ADL: if there is unfilled remainder, ADL against counter-positions
        // at the bankruptcy price.
        if unfilled.is_non_zero() {
            // Snapshot the user's margin before ADL to measure the realized
            // PnL delta for the `Liquidated` event (liq trading fees are
            // zero, so the margin delta equals the realized PnL from ADL).
            let user_margin_pre_adl = user_state.margin;

            let (adl_size, adl_price, adl_funding) = execute_adl(
                storage,
                contract,
                &liq_param,
                &mut state,
                user,
                pair_id,
                pair_state,
                user_state,
                maker_states,
                unfilled,
                bankruptcy_price,
                &mut all_volumes,
                &mut all_index_updates,
                events,
            )?;

            // Add ADL notional.
            closed_notional
                .checked_add_assign(adl_size.checked_abs()?.checked_mul(oracle_price)?)?;

            // Margin delta = closing PnL + funding settled. Subtract funding
            // to get the closing-only `adl_realized_pnl` for v0.17.0+.
            let adl_realized_pnl = user_state
                .margin
                .checked_sub(user_margin_pre_adl)?
                .checked_sub(adl_funding)?;

            events.push(Liquidated {
                user,
                pair_id: pair_id.clone(),
                adl_size,
                adl_price: Some(adl_price),
                adl_realized_pnl,
                adl_realized_funding: Some(adl_funding),
                remaining_position_size: Some(
                    user_state
                        .positions
                        .get(pair_id)
                        .map(|p| p.size)
                        .unwrap_or_default(),
                ),
            })?;

            #[cfg(feature = "metrics")]
            {
                metrics::counter!(
                    crate::metrics::LABEL_ADL_EVENTS,
                    "pair_id" => pair_id.to_string()
                )
                .increment(1);
            }
        } else {
            events.push(Liquidated {
                user,
                pair_id: pair_id.clone(),
                adl_size: Quantity::ZERO,
                adl_price: None,
                adl_realized_pnl: UsdValue::ZERO,
                adl_realized_funding: Some(UsdValue::ZERO),
                remaining_position_size: Some(
                    user_state
                        .positions
                        .get(pair_id)
                        .map(|p| p.size)
                        .unwrap_or_default(),
                ),
            })?;
        }
    }

    Ok((
        state,
        all_fee_breakdowns,
        all_order_mutations,
        closed_notional,
        all_index_updates,
        all_volumes,
        next_order_id,
        next_fill_id,
    ))
}

/// ADL the unfilled remainder of a liquidation against counter-positions.
///
/// Returns: `(total_adl_size, bankruptcy_price, total_user_funding)` —
/// where `total_user_funding` is the funding settled on the liquidated
/// user's position across all of this call's per-fill `settle_fill`s,
/// summed into a single value for `Liquidated.adl_realized_funding`.
///
/// This is a leaf helper private to `execute_close_schedule` and keeps `&mut`
/// parameters by design; it is not part of the pure set.
#[allow(clippy::too_many_arguments)]
fn execute_adl(
    storage: &dyn Storage,
    contract: Addr,
    param: &Param,
    state: &mut State,
    user: Addr,
    pair_id: &PairId,
    pair_state: &mut PairState,
    user_state: &mut UserState,
    maker_states: &mut BTreeMap<Addr, UserState>,
    unfilled: Quantity,
    bankruptcy_price: UsdPrice,
    all_volumes: &mut BTreeMap<Addr, UsdValue>,
    index_updates: &mut Vec<PositionIndexUpdate>,
    events: &mut EventBuilder,
) -> anyhow::Result<(Quantity, UsdPrice, UsdValue)> {
    let mut remaining = unfilled;
    let taker_is_selling = unfilled.is_negative();

    // Collect counter-parties to avoid borrow conflicts during iteration.
    let counter_parties: Vec<(UsdPrice, Addr)> = if taker_is_selling {
        // Need shorts (buyers). Most profitable shorts have highest entry price.
        SHORTS
            .prefix(pair_id.clone())
            .keys(storage, None, None, IterationOrder::Descending)
            .collect::<Result<_, _>>()?
    } else {
        // Need longs (sellers). Most profitable longs have lowest entry price.
        LONGS
            .prefix(pair_id.clone())
            .keys(storage, None, None, IterationOrder::Ascending)
            .collect::<Result<_, _>>()?
    };

    let mut total_adl_size = Quantity::ZERO;
    let mut total_user_funding = UsdValue::ZERO;

    for (entry_price, counter_user) in counter_parties {
        if remaining.is_zero() {
            break;
        }

        // Skip the user being liquidated (they can't be their own counter-party).
        if counter_user == user {
            continue;
        }

        // Take the counter-party's state out of the map so the later
        // `settle_pnls` can borrow the vault state disjointly (or simply
        // use the counter-party's state when the counter-party *is* the
        // vault).
        let mut counter_state = match maker_states.remove(&counter_user) {
            Some(s) => s,
            None => USER_STATES
                .may_load(storage, counter_user)?
                .unwrap_or_default(),
        };

        // Verify counter-party still has this position (may have been modified
        // by earlier book matching in a shared maker_states map).
        let counter_position = match counter_state.positions.get(pair_id) {
            Some(pos) if pos.entry_price == entry_price => pos,
            _ => {
                // Put the counter-party's state back unchanged.
                maker_states.insert(counter_user, counter_state);
                continue;
            },
        };

        let user_close = {
            let counter_size = counter_position.size.checked_abs()?;
            let fill_amount = remaining.checked_abs()?.min(counter_size);
            if taker_is_selling {
                fill_amount.checked_neg()?
            } else {
                fill_amount
            }
        };

        // User side — position update and scalar PnL / fee / volume.
        let old_user_pos = user_state.positions.get(pair_id).cloned();
        let user_settlement = settle_fill(
            contract,
            pair_id,
            pair_state,
            user_state,
            user,
            user_close,
            bankruptcy_price,
            Dimensionless::ZERO,
            None,
        )?;

        total_user_funding.checked_add_assign(user_settlement.pnl.funding)?;

        all_volumes
            .entry(user)
            .or_default()
            .checked_add_assign(user_settlement.volume)?;

        if let Some(diff) = compute_position_diff(
            pair_id,
            user,
            old_user_pos.as_ref(),
            user_state.positions.get(pair_id),
        ) {
            index_updates.push(diff);
        }

        // Counter-party side — position update and scalar PnL / fee / volume.
        let old_counter_pos = counter_state.positions.get(pair_id).cloned();
        let counter_settlement = settle_fill(
            contract,
            pair_id,
            pair_state,
            &mut counter_state,
            counter_user,
            user_close.checked_neg()?,
            bankruptcy_price,
            Dimensionless::ZERO,
            None,
        )?;
        all_volumes
            .entry(counter_user)
            .or_default()
            .checked_add_assign(counter_settlement.volume)?;
        if let Some(diff) = compute_position_diff(
            pair_id,
            counter_user,
            old_counter_pos.as_ref(),
            counter_state.positions.get(pair_id),
        ) {
            index_updates.push(diff);
        }

        // Per-fill settlement on margins. All fees are zero during ADL,
        // so this is effectively a PnL application with a no-op fee
        // split. `vault_state_opt` is None when either party IS the vault
        // (their own state then routes the zero vault fee); otherwise we
        // look up the vault's state in the maker-states map.
        {
            let vault_state_opt = if user != contract && counter_user != contract {
                Some(maker_states.entry(contract).or_insert_with(|| {
                    USER_STATES
                        .may_load(storage, contract)
                        .unwrap()
                        .unwrap_or_default()
                }))
            } else {
                None
            };
            // `_fill_breakdowns` is empty because both fees are zero.
            let _fill_breakdowns = settle_pnls(
                contract,
                param,
                state,
                user,
                user_state,
                user_settlement.pnl.total()?,
                user_settlement.fee,
                counter_user,
                &mut counter_state,
                counter_settlement.pnl.total()?,
                counter_settlement.fee,
                vault_state_opt,
            )?;
        }

        // Emit Deleveraged event for counter-party. The closing /
        // funding split lives on `pnl` directly (no inline arithmetic).
        events.push(Deleveraged {
            user: counter_user,
            pair_id: pair_id.clone(),
            closing_size: user_close.checked_neg()?,
            fill_price: bankruptcy_price,
            realized_pnl: counter_settlement.pnl.closing,
            realized_funding: Some(counter_settlement.pnl.funding),
            remaining_position_size: Some(
                counter_state
                    .positions
                    .get(pair_id)
                    .map(|p| p.size)
                    .unwrap_or_default(),
            ),
        })?;

        remaining = remaining.checked_sub(user_close)?;
        total_adl_size.checked_add_assign(user_close)?;

        // Put the counter-party's (now updated) state back.
        maker_states.insert(counter_user, counter_state);
    }

    Ok((total_adl_size, bankruptcy_price, total_user_funding))
}

/// Compute the liquidation fee, capped at the user's remaining equity.
///
/// Capping at equity (margin + unrealized PnL of remaining positions), rather
/// than margin alone, guarantees the fee never pushes equity below zero and so
/// can never itself create bad debt.
///
/// Caller guarantees `user_equity` already reflects realized PnL from the
/// liquidation fills (per-fill settlement applies PnLs as fills happen).
fn compute_liquidation_fee(
    closed_notional: UsdValue,
    liquidation_fee_rate: Dimensionless,
    user_equity: UsdValue,
) -> anyhow::Result<UsdValue> {
    let fee_usd = closed_notional.checked_mul(liquidation_fee_rate)?;
    let remaining_equity = user_equity.max(UsdValue::ZERO);
    Ok(fee_usd.min(remaining_equity))
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::state::{
            FEE_RATE_OVERRIDES, LONGS, PAIR_PARAMS, PAIR_STATES, PARAM, SHORTS, STATE, USER_STATES,
        },
        dango_math::Uint64,
        dango_order_book::{
            ChildOrder, Dimensionless, FundingPerUnit, LimitOrder, OrderKey, Quantity, UsdPrice,
            UsdValue, may_invert_price,
        },
        dango_primitives::{Addr, Coins, MockContext, Storage, Timestamp},
        dango_types::perps::{PairParam, PairState, Param, Position, State, UserState},
        std::collections::BTreeMap,
    };

    const USER: Addr = Addr::mock(1);
    const MAKER: Addr = Addr::mock(2);
    const COUNTER: Addr = Addr::mock(3);
    const CONTRACT: Addr = Addr::mock(0);

    fn pair_btc() -> PairId {
        "perp/btcusd".parse().unwrap()
    }

    fn default_param() -> Param {
        Param {
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            liquidation_fee_rate: Dimensionless::new_permille(10), // 1%
            max_open_orders: 100,
            ..Default::default()
        }
    }

    fn btc_pair_param() -> PairParam {
        PairParam {
            initial_margin_ratio: Dimensionless::new_permille(100), // 10%
            maintenance_margin_ratio: Dimensionless::new_permille(50), // 5%
            max_abs_oi: Quantity::new_int(1_000_000),
            max_limit_price_deviation: Dimensionless::new_permille(500), // 50%
            max_market_slippage: Dimensionless::new_permille(500),       // 50%
            ..Default::default()
        }
    }

    /// Set up the contract storage with pair params, pair states, and global params.
    fn setup_storage(
        storage: &mut dyn Storage,
        param: &Param,
        pairs: &[(PairId, PairParam, PairState)],
    ) {
        PARAM.save(storage, param).unwrap();
        STATE.save(storage, &State::default()).unwrap();
        NEXT_ORDER_ID.save(storage, &OrderId::ONE).unwrap();
        NEXT_FILL_ID.save(storage, &FillId::ONE).unwrap();

        for (pair_id, pair_param, pair_state) in pairs {
            PAIR_PARAMS.save(storage, pair_id, pair_param).unwrap();
            PAIR_STATES.save(storage, pair_id, pair_state).unwrap();
        }
    }

    /// Save a user position and add to LONGS/SHORTS index.
    fn save_position(
        storage: &mut dyn Storage,
        user: Addr,
        pair_id: &PairId,
        size: i128,
        entry_price: i128,
    ) {
        let mut user_state = USER_STATES
            .may_load(storage, user)
            .unwrap()
            .unwrap_or_default();

        user_state.positions.insert(
            pair_id.clone(),
            Position {
                size: Quantity::new_int(size),
                entry_price: UsdPrice::new_int(entry_price),
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );

        USER_STATES.save(storage, user, &user_state).unwrap();

        // Update position index.
        let ep = UsdPrice::new_int(entry_price);
        if size > 0 {
            LONGS.insert(storage, (pair_id.clone(), ep, user)).unwrap();
        } else {
            SHORTS.insert(storage, (pair_id.clone(), ep, user)).unwrap();
        }
    }

    /// Save a bid order into the book (buy side).
    /// Bids are stored with inverted price so ascending iteration gives best bid first.
    fn save_bid(
        storage: &mut dyn Storage,
        pair_id: &PairId,
        order_id: u64,
        maker: Addr,
        size: i128,
        price: i128,
    ) {
        let stored_price = may_invert_price(UsdPrice::new_int(price), true);
        let key: OrderKey = (pair_id.clone(), stored_price, Uint64::new(order_id));
        let order = LimitOrder {
            user: maker,
            size: Quantity::new_int(size),
            reduce_only: false,
            reserved_margin: UsdValue::ZERO,
            created_at: Timestamp::ZERO,
            tp: None,
            sl: None,
            client_order_id: None,
        };
        BIDS.save(storage, key, &order).unwrap();
    }

    /// Save an ask order into the book (sell side).
    fn save_ask(
        storage: &mut dyn Storage,
        pair_id: &PairId,
        order_id: u64,
        maker: Addr,
        size: i128,
        price: i128,
    ) {
        let stored_price = UsdPrice::new_int(price);
        let key: OrderKey = (pair_id.clone(), stored_price, Uint64::new(order_id));
        let order = LimitOrder {
            user: maker,
            size: Quantity::new_int(size),
            reduce_only: false,
            reserved_margin: UsdValue::ZERO,
            created_at: Timestamp::ZERO,
            tp: None,
            sl: None,
            client_order_id: None,
        };
        ASKS.save(storage, key, &order).unwrap();
    }

    // ======================== Tests ========================

    #[test]
    fn not_liquidatable_errors() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = default_param();
        let pair_state = PairState {
            index_price: UsdPrice::new_int(50_000),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state)],
        );

        // User has long 1 BTC at 50000, oracle at 50000.
        // Collateral = 10000 USD (well above MM = 50000 * 5% = 2500).
        save_position(&mut ctx.storage, USER, &pair_btc(), 1, 50_000);

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(
            pair_btc(),
            PairState {
                index_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
        );

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(50_000));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(10_000);

        let state = STATE.load(&ctx.storage).unwrap();

        let result = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        );

        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("not liquidatable"),
            "expected 'not liquidatable' error"
        );
    }

    #[test]
    fn single_position_full_close_on_book() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = default_param();
        let pair_state = PairState {
            long_oi: Quantity::new_int(10),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // User has long 10 BTC at entry 50000. Oracle is now 47500.
        // equity < MM → liquidatable
        save_position(&mut ctx.storage, USER, &pair_btc(), 10, 50_000);

        // Maker with bids to absorb the liquidation (user is selling a long).
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();

        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 10, 47_500);

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(47_500));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(2_400);

        let state = STATE.load(&ctx.storage).unwrap();

        let LiquidateOutcome { user_state, .. } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("liquidation should succeed");

        // User's position should be closed.
        assert!(
            user_state.positions.is_empty(),
            "user should have no positions after liquidation"
        );
    }

    #[test]
    fn single_position_full_adl_empty_book() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = default_param();
        let pair_state = PairState {
            long_oi: Quantity::new_int(10),
            short_oi: Quantity::new_int(10),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // User long 10 BTC at 50000, oracle 47500 → liquidatable.
        save_position(&mut ctx.storage, USER, &pair_btc(), 10, 50_000);

        // Counter-party has short 10 BTC at $55000 (profitable).
        save_position(&mut ctx.storage, COUNTER, &pair_btc(), -10, 55_000);
        let mut counter_state = USER_STATES.load(&ctx.storage, COUNTER).unwrap();
        counter_state.margin = UsdValue::new_int(100_000);
        USER_STATES
            .save(&mut ctx.storage, COUNTER, &counter_state)
            .unwrap();

        // No book orders → ADL should happen.

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(47_500));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(2_400);

        let state = STATE.load(&ctx.storage).unwrap();

        let result = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        );

        assert!(result.is_ok(), "ADL failed: {:?}", result.err());

        let LiquidateOutcome {
            user_state,
            maker_states,
            ..
        } = result.unwrap();

        // User's position should be closed.
        assert!(user_state.positions.is_empty());

        // Counter-party's position should be reduced/closed.
        let counter_final = &maker_states[&COUNTER];
        assert!(
            !counter_final.positions.contains_key(&pair_btc())
                || counter_final.positions[&pair_btc()]
                    .size
                    .checked_abs()
                    .unwrap()
                    < Quantity::new_int(10),
            "counter-party position should be reduced"
        );
    }

    #[test]
    fn liquidation_fee_to_insurance_fund() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = default_param();
        let pair_state = PairState {
            long_oi: Quantity::new_int(1),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // User long 1 BTC @ $50,000, margin $2,500, oracle $48,000.
        // Equity = $2,500 + ($48,000-$50,000) = $500.
        // MM = $48,000 * 5% = $2,400. Equity < MM → liquidatable.
        save_position(&mut ctx.storage, USER, &pair_btc(), 1, 50_000);

        // Bid on book at $49,000 (better than bp=$47,500).
        // Fill at $49,000 leaves margin for the liq fee.
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 1, 49_000);

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(48_000));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(2_500);

        let state = STATE.load(&ctx.storage).unwrap();

        let LiquidateOutcome { state, .. } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("liq fee test should succeed");

        // Fill at $49,000: PnL = 1*($49,000-$50,000) = -$1,000.
        // Remaining margin = $2,500 - $1,000 = $1,500.
        // Liq fee = min($49,000 * 1%, $1,500) = min($490, $1,500) = $490.
        // Insurance fund should have received the fee.
        assert!(
            state.insurance_fund > UsdValue::ZERO,
            "insurance fund should have received the liquidation fee"
        );
    }

    /// Verifies that the liquidation fee goes entirely to the insurance fund
    /// with NO portion leaking to the protocol treasury, even when
    /// `protocol_fee_rate` is non-zero.
    ///
    /// Old behavior: the liq fee was added to `all_fees` and processed by
    /// `settle_pnls`, which split it via `protocol_fee_rate`. With a 20%
    /// protocol rate, `settle_pnls` sent $96 (20% of $480) to treasury and
    /// $384 (80%) to vault. The post-settle code then subtracted the full
    /// $480 from the vault and credited $480 to the insurance fund.
    ///
    /// Incorrect outcome: treasury gained $96 it should not have, and the
    /// vault lost $96 (received $384 then had $480 subtracted). The
    /// insurance fund got the right total ($480), but the vault silently
    /// subsidized a phantom treasury cut on every liquidation.
    ///
    /// Correct behavior: the liq fee is deducted from `user_state.margin`
    /// and credited to `insurance_fund` directly before `settle_pnls`,
    /// bypassing the protocol/vault fee split entirely.
    ///
    /// Correct expected outcome: insurance_fund += $480, treasury unchanged,
    /// vault margin unchanged.
    ///
    /// Setup:
    ///   - User long 1 BTC @ $50,000, margin $2,000, oracle $48,000.
    ///   - Equity = $2,000 + ($48,000 − $50,000) = $0. MM = $48,000 × 5% = $2,400.
    ///   - Equity ($0) < MM ($2,400) → liquidatable. Full close.
    ///   - `liquidation_fee_rate = 1%`, `protocol_fee_rate = 20%`.
    ///   - Book bid at $49,000 → fills at $49,000.
    ///
    /// Expected:
    ///   - PnL = ($49,000 − $50,000) × 1 = −$1,000.
    ///   - closed_notional = 1 × $48,000 (oracle) = $48,000.
    ///   - fee_usd = $48,000 × 1% = $480.
    ///   - remaining_margin = max(0, $2,000 + (−$1,000)) = $1,000.
    ///   - liq_fee = min($480, $1,000) = $480.
    ///   - insurance_fund += $480, treasury = $0, vault unchanged.
    #[test]
    fn liquidation_fee_no_treasury_leak() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = Param {
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            liquidation_fee_rate: Dimensionless::new_permille(10), // 1%
            protocol_fee_rate: Dimensionless::new_percent(20),     // 20%
            max_open_orders: 100,
            ..Default::default()
        };

        let pair_state = PairState {
            long_oi: Quantity::new_int(1),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // User long 1 BTC @ $50,000, margin $2,000, oracle $48,000.
        // Equity = $0, deficit = $2,400 → full position closed.
        save_position(&mut ctx.storage, USER, &pair_btc(), 1, 50_000);

        // Bid on book at $49,000 from maker.
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 1, 49_000);

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(48_000));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(2_000);

        let state = STATE.load(&ctx.storage).unwrap();

        // Snapshot pre-liquidation values.
        let treasury_before = state.treasury;
        let insurance_before = state.insurance_fund;
        let vault_margin_before = USER_STATES
            .may_load(&ctx.storage, CONTRACT)
            .unwrap()
            .unwrap_or_default()
            .margin;

        let LiquidateOutcome {
            state,
            maker_states,
            ..
        } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("liquidation should succeed");

        // closed_notional = 1 × $48,000 = $48,000.
        // liq_fee = min($480, $1,000) = $480.
        let expected_liq_fee = UsdValue::new_int(480);

        // Only the insurance fund should change — by exactly the liq fee.
        assert_eq!(
            state.insurance_fund,
            insurance_before.checked_add(expected_liq_fee).unwrap(),
            "insurance fund should increase by exactly the liquidation fee"
        );

        // The protocol treasury must be unchanged.
        assert_eq!(
            state.treasury, treasury_before,
            "treasury must not change — liquidation fee should not be split"
        );

        // The vault margin must be unchanged.
        let vault_margin_after = maker_states
            .get(&CONTRACT)
            .map(|vs| vs.margin)
            .unwrap_or(vault_margin_before);
        assert_eq!(
            vault_margin_after, vault_margin_before,
            "vault margin must not change from liquidation fee routing"
        );
    }

    /// Verifies the "liquidation fills are fee-free" invariant holds for a
    /// maker that has an admin-configured fee rate override.
    ///
    /// `match_order` looks up `FEE_RATE_OVERRIDES` before falling back to
    /// `param.maker_fee_rates`, so without the `force_maker_fee_rate`
    /// argument, the override would defeat `liq_param`'s zero schedule and
    /// the maker would be charged a trading fee during liquidation — a
    /// regression against `book/perps/4-liquidation-and-adl.md §4`.
    ///
    /// Setup:
    ///   - User long 1 BTC @ $50,000, margin $2,500, oracle $48,000.
    ///   - Equity = $500. MM = $2,400. Equity < MM → liquidatable.
    ///   - Book bid at $49,000 from MAKER, size 1.
    ///   - MAKER has a fee rate override = (1% maker, 1% taker).
    ///   - `protocol_fee_rate = 20%` so any leaked fee splits into both
    ///     treasury and vault, making leakage maximally observable.
    ///
    /// Expected (with the fix):
    ///   - MAKER's side of the fill is pure opening (PnL = 0).
    ///   - Maker fee = 0 → MAKER.margin unchanged ($100,000).
    ///   - Treasury unchanged (no protocol cut from a zero fee).
    ///   - Vault margin unchanged (no vault cut from a zero fee).
    ///
    /// Without the fix, a nonzero maker fee leaks through regardless of
    /// the exact close size, so MAKER.margin < $100,000 and the vault /
    /// treasury pick up an 80/20 split of that fee. The equality asserts
    /// below catch any nonzero leak.
    #[test]
    fn liquidation_respects_zero_fee_even_with_maker_override() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = Param {
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            liquidation_fee_rate: Dimensionless::new_permille(10), // 1%
            protocol_fee_rate: Dimensionless::new_percent(20),     // 20%
            max_open_orders: 100,
            ..Default::default()
        };

        let pair_state = PairState {
            long_oi: Quantity::new_int(1),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        save_position(&mut ctx.storage, USER, &pair_btc(), 1, 50_000);

        // MAKER has an active fee rate override — this is exactly the state
        // the fix must tolerate during a liquidation.
        FEE_RATE_OVERRIDES
            .save(
                &mut ctx.storage,
                MAKER,
                &(
                    Dimensionless::new_permille(10), // 1% maker
                    Dimensionless::new_permille(10), // 1% taker (irrelevant here)
                ),
            )
            .unwrap();

        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 1, 49_000);

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(48_000));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(2_500);

        let state = STATE.load(&ctx.storage).unwrap();

        let treasury_before = state.treasury;
        let vault_margin_before = USER_STATES
            .may_load(&ctx.storage, CONTRACT)
            .unwrap()
            .unwrap_or_default()
            .margin;

        let LiquidateOutcome {
            state,
            maker_states,
            ..
        } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("liquidation should succeed");

        // MAKER opened a 1 BTC long at $49,000 (pure opening fill). With
        // the override bypassed, there is no trading fee to deduct.
        assert_eq!(
            maker_states[&MAKER].margin,
            UsdValue::new_int(100_000),
            "maker margin must be unchanged — no trading fee during liquidation"
        );

        // No fee was charged, so neither the treasury nor the vault takes
        // a cut. (The liq fee itself bypasses both and lands in the
        // insurance fund.)
        assert_eq!(
            state.treasury, treasury_before,
            "treasury must not receive a protocol cut — no maker fee was charged"
        );
        assert_eq!(
            maker_states[&CONTRACT].margin, vault_margin_before,
            "vault must not receive a maker-fee cut during liquidation"
        );
    }

    /// Verifies that when the vault itself is liquidated (`user == contract`),
    /// the liquidation fee is correctly deducted from the vault's own margin
    /// and credited to the insurance fund.
    ///
    /// Old behavior: the liq fee was added to `all_fees` and processed by
    /// `settle_pnls`, which split it between treasury and vault. For normal
    /// users, the post-settle code reversed the vault credit. But when
    /// `user == contract`, `settle_pnls` skipped the fee entirely (because
    /// of the `user == contract` guard at line 1036 of submit_order.rs),
    /// and the old post-settle code deducted the fee from `user_state.margin`
    /// directly — which happened to produce the right result for the vault
    /// case, but only by accident, relying on a fragile two-branch structure.
    ///
    /// Correct behavior: the liq fee is deducted from `user_state.margin`
    /// and credited to `insurance_fund` directly before `settle_pnls`,
    /// bypassing the fee split entirely. This works identically whether
    /// the user is a normal account or the vault itself.
    ///
    /// Setup:
    ///   - Vault (CONTRACT) long 1 BTC @ $50,000, margin $2,000, oracle $48,000.
    ///   - Equity = $2,000 + ($48,000 − $50,000) = $0. MM = $48,000 × 5% = $2,400.
    ///   - Equity ($0) < MM ($2,400) → liquidatable. Full close.
    ///   - `liquidation_fee_rate = 1%`, `protocol_fee_rate = 0%`.
    ///   - Book bid at $49,000 from MAKER → fills at $49,000.
    ///
    /// Expected:
    ///   - PnL = ($49,000 − $50,000) × 1 = −$1,000.
    ///   - closed_notional = 1 × $48,000 = $48,000.
    ///   - remaining_margin = max(0, $2,000 + (−$1,000)) = $1,000.
    ///   - liq_fee = min($480, $1,000) = $480.
    ///   - insurance_fund += $480, treasury unchanged.
    ///   - Vault margin = $2,000 − $480 (liq_fee) + (−$1,000) (PnL) = $520.
    ///   - Vault must NOT appear in maker_states (it is the taker).
    #[test]
    fn vault_self_liquidation_fee_to_insurance() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = default_param();

        let pair_state = PairState {
            long_oi: Quantity::new_int(1),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // Vault (CONTRACT) long 1 BTC @ $50,000.
        save_position(&mut ctx.storage, CONTRACT, &pair_btc(), 1, 50_000);

        // Bid on book at $49,000 from MAKER.
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 1, 49_000);

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(48_000));

        let mut user_state = USER_STATES.load(&ctx.storage, CONTRACT).unwrap();
        user_state.margin = UsdValue::new_int(2_000);

        let state = STATE.load(&ctx.storage).unwrap();

        let treasury_before = state.treasury;
        let insurance_before = state.insurance_fund;

        let LiquidateOutcome {
            state,
            user_state,
            maker_states,
            ..
        } = _liquidate(
            &ctx.storage,
            CONTRACT, // vault is the user being liquidated
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("vault self-liquidation should succeed");

        let expected_liq_fee = UsdValue::new_int(480);

        assert_eq!(
            state.insurance_fund,
            insurance_before.checked_add(expected_liq_fee).unwrap(),
            "insurance fund should increase by exactly the liquidation fee"
        );

        assert_eq!(
            state.treasury, treasury_before,
            "treasury must not change during vault self-liquidation"
        );

        // Vault margin: $2,000 - $480 (liq_fee) + (-$1,000) (PnL) = $520.
        assert_eq!(
            user_state.margin,
            UsdValue::new_int(520),
            "vault margin should reflect liq_fee deduction and PnL settlement"
        );

        // Vault must NOT appear in maker_states when it is the taker.
        assert!(
            !maker_states.contains_key(&CONTRACT),
            "vault must not be in maker_states when it is the liquidated user"
        );

        // Position should be fully closed.
        assert!(
            user_state.positions.is_empty(),
            "vault position should be fully closed after liquidation"
        );
    }

    /// Verifies that when the uncapped liquidation fee exceeds the user's
    /// remaining margin, the fee is capped at `remaining_margin` and the
    /// capped amount goes entirely to the insurance fund with no treasury
    /// leak, even when `protocol_fee_rate` is nonzero.
    ///
    /// Old behavior: the liq fee was added to `all_fees` and processed by
    /// `settle_pnls`, which split it via `protocol_fee_rate`. With a 20%
    /// protocol rate, `settle_pnls` sent 20% to treasury and 80% to vault.
    /// The post-settle code then subtracted the full `liq_fee` from the
    /// vault and credited it to the insurance fund. Result: treasury kept
    /// `liq_fee × 20%` and the vault lost that same amount — a silent
    /// leak from vault to treasury on every liquidation.
    ///
    /// Correct behavior: the liq fee is deducted from `user_state.margin`
    /// and credited to `insurance_fund` directly before `settle_pnls`,
    /// bypassing the protocol/vault fee split. Treasury and vault are
    /// unaffected by the liquidation fee.
    ///
    /// Setup:
    ///   - User long 1 BTC @ $50,000, margin $1,200, oracle $48,000.
    ///   - Equity = $1,200 + ($48,000 − $50,000) = −$800. MM = $2,400.
    ///   - Equity (−$800) < MM ($2,400) → liquidatable. Full close.
    ///   - `liquidation_fee_rate = 1%`, `protocol_fee_rate = 20%`.
    ///   - Book bid at $49,000 → fills at $49,000.
    ///
    /// Expected:
    ///   - PnL = ($49,000 − $50,000) × 1 = −$1,000.
    ///   - closed_notional = 1 × $48,000 = $48,000.
    ///   - Uncapped fee = $48,000 × 1% = $480.
    ///   - remaining_margin = max(0, $1,200 + (−$1,000)) = $200.
    ///   - liq_fee = min($480, $200) = $200 (cap binds).
    ///   - insurance_fund += $200, treasury unchanged, vault unchanged.
    ///   - User margin = $1,200 − $200 (liq_fee) + (−$1,000) (PnL) = $0.
    #[test]
    fn liquidation_fee_capped_no_treasury_leak() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = Param {
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(10), // 1%
                ..Default::default()
            },
            liquidation_fee_rate: Dimensionless::new_permille(10), // 1%
            protocol_fee_rate: Dimensionless::new_percent(20),     // 20%
            max_open_orders: 100,
            ..Default::default()
        };

        let pair_state = PairState {
            long_oi: Quantity::new_int(1),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // User long 1 BTC @ $50,000, margin $1,200, oracle $48,000.
        // Equity = -$800, deficit large → full position closed.
        save_position(&mut ctx.storage, USER, &pair_btc(), 1, 50_000);

        // Bid on book at $49,000 from maker.
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 1, 49_000);

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(48_000));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(1_200);

        let state = STATE.load(&ctx.storage).unwrap();

        // Snapshot pre-liquidation values.
        let treasury_before = state.treasury;
        let insurance_before = state.insurance_fund;
        let vault_margin_before = USER_STATES
            .may_load(&ctx.storage, CONTRACT)
            .unwrap()
            .unwrap_or_default()
            .margin;

        let LiquidateOutcome {
            state,
            user_state,
            maker_states,
            ..
        } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("liquidation should succeed");

        // Uncapped fee = $480, remaining_margin = $200 → capped at $200.
        let expected_liq_fee = UsdValue::new_int(200);

        // Only the insurance fund should change — by the capped fee.
        assert_eq!(
            state.insurance_fund,
            insurance_before.checked_add(expected_liq_fee).unwrap(),
            "insurance fund should increase by the capped liquidation fee ($200, not $480)"
        );

        // Treasury must be unchanged despite 20% protocol_fee_rate.
        assert_eq!(
            state.treasury, treasury_before,
            "treasury must not change — capped liquidation fee should not be split"
        );

        // Vault margin must be unchanged.
        let vault_margin_after = maker_states
            .get(&CONTRACT)
            .map(|vs| vs.margin)
            .unwrap_or(vault_margin_before);
        assert_eq!(
            vault_margin_after, vault_margin_before,
            "vault margin must not change from liquidation fee routing"
        );

        // User margin: $1,200 - $200 (liq_fee) + (-$1,000) (PnL) = $0 exactly.
        assert_eq!(
            user_state.margin,
            UsdValue::ZERO,
            "user margin should be exactly zero — no bad debt"
        );

        // Position should be fully closed.
        assert!(
            user_state.positions.is_empty(),
            "user position should be fully closed after liquidation"
        );
    }

    #[test]
    fn bad_debt_covered_by_insurance_fund() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = Param {
            liquidation_fee_rate: Dimensionless::ZERO,
            ..default_param()
        };
        let pair_state = PairState {
            long_oi: Quantity::new_int(10),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // User long 10 BTC @ $50,000, margin $2,400, oracle $47,500.
        // PnL = 10*(47500-50000) = -$25,000. Equity = $2,400 - $25,000 = -$22,600.
        save_position(&mut ctx.storage, USER, &pair_btc(), 10, 50_000);

        // Bid on book at oracle price from maker. When the liquidation taker
        // sells into this bid, the fill realizes the full unrealized loss,
        // driving margin negative → bad debt.
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 10, 47_500);

        // Seed insurance fund with $500.
        let mut state = STATE.load(&ctx.storage).unwrap();
        state.insurance_fund = UsdValue::new_int(500);
        STATE.save(&mut ctx.storage, &state).unwrap();

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(47_500));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(2_400);

        let LiquidateOutcome {
            state, user_state, ..
        } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("bad debt test should succeed");

        // User margin should be floored at zero.
        assert_eq!(user_state.margin, UsdValue::ZERO);

        // Fill at $47,500: PnL = 10*($47,500-$50,000) = -$25,000.
        // Margin = $2,400 - $25,000 = -$22,600. Bad debt = $22,600.
        // Insurance fund = $500 - $22,600 = -$22,100.
        assert!(
            state.insurance_fund < UsdValue::new_int(500),
            "insurance fund should have been reduced by bad debt"
        );
    }

    /// A cross-margined account can reach liquidation with NEGATIVE margin but
    /// POSITIVE equity — e.g. the user withdrew, or realised a loss, against an
    /// open position's unrealised profit. The remaining position keeps equity
    /// above zero, so there is no bad debt: the engine must not mistake the
    /// negative margin balance for insolvency and drain the insurance fund. It
    /// must also still charge a liquidation fee, capped at equity rather than
    /// the (negative) margin.
    ///
    /// Mirrors the "negative margin, positive equity" example in
    /// `book/perps/4-liquidation-and-adl.md`.
    #[test]
    fn no_bad_debt_when_equity_positive_despite_negative_margin() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        // 0.1% liquidation fee, as in the spec examples.
        let param = Param {
            liquidation_fee_rate: Dimensionless::new_permille(1),
            ..default_param()
        };
        // `index_price` must match `oracle_prices` below: the deficit /
        // liquidatable computation reads the stored pair state, while the close
        // execution reads the `oracle_prices` map — a mismatch would mis-size
        // the close.
        let pair_state = PairState {
            long_oi: Quantity::new_int(10),
            index_price: UsdPrice::new_int(2_200),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // Alice: long 10 BTC @ $2,000, oracle $2,200 (position in profit). Her
        // margin is set negative to model a prior withdrawal / loss realised
        // against that unrealised profit:
        //
        //   equity = -1,450 + 10*(2,200-2,000) = 550   (> 0, solvent)
        //   MM     = 10*2,200*5%               = 1,100 (> equity, liquidatable)
        save_position(&mut ctx.storage, USER, &pair_btc(), 10, 2_000);

        // Maker bid at the $2,200 oracle so the partial close fills on the book
        // (no ADL).
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 10, 2_200);

        // Seed the insurance fund with $100 so we can prove it GREW by the fee
        // and was never debited for phantom bad debt.
        let mut state = STATE.load(&ctx.storage).unwrap();
        state.insurance_fund = UsdValue::new_int(100);
        STATE.save(&mut ctx.storage, &state).unwrap();

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(2_200));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(-1_450);

        let LiquidateOutcome {
            state, user_state, ..
        } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("liquidation should succeed");

        // Close schedule: deficit = 1,100 - 550 = 550 ⇒ close 5 of 10 BTC.
        // Book fill at $2,200: PnL = 5*(2,200-2,000) = +1,000 ⇒ margin -450.
        // Fee = 5*2,200*0.1% = 11, capped at equity 550 ⇒ margin -461.
        // Equity after = -461 + 5*200 = 539 > 0 ⇒ no bad debt.

        // Margin stays negative — it must NOT be floored to zero.
        assert_eq!(
            user_state.margin,
            UsdValue::new_int(-461),
            "margin must remain negative; the account is solvent, not bad debt"
        );

        // 5 BTC remain (partial close).
        assert_eq!(user_state.positions[&pair_btc()].size, Quantity::new_int(5));

        // Insurance fund grew by the fee (100 + 11) and was never debited for
        // phantom bad debt. The bug would have produced 100 + 0 - 450 = -350.
        assert_eq!(state.insurance_fund, UsdValue::new_int(111));
    }

    /// Proves the cancel-before-match invariant: when a user is liquidated,
    /// their resting orders are removed *before* the close schedule matches
    /// against the book, so self-trades cannot occur.
    #[test]
    fn liquidation_cancels_user_orders_before_matching() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = default_param();
        let pair_state = PairState {
            long_oi: Quantity::new_int(1),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // USER has long 1 BTC at $50,000. Oracle at $47,500 → deeply underwater.
        // Equity = $100 + ($47,500-$50,000) = -$2,400.
        // MM = $47,500 * 5% = $2,375. Equity < MM → liquidatable.
        // Deficit = $2,375 - (-$2,400) = $4,775 → full close (close_amount ≥ 1).
        save_position(&mut ctx.storage, USER, &pair_btc(), 1, 50_000);

        // USER also has a resting ask (sell) on the book — this must be
        // cancelled before matching, otherwise it could self-match.
        save_ask(&mut ctx.storage, &pair_btc(), 10, USER, -1, 48_000);

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(100);
        user_state.open_order_count = 1;
        USER_STATES
            .save(&mut ctx.storage, USER, &user_state)
            .unwrap();

        // MAKER has a bid to absorb the liquidation close (user sells long).
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();
        save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 1, 47_500);

        // Step 1: Cancel all user orders (mirrors the public `liquidate` flow).
        let mut events = EventBuilder::new();
        let CancelAllOrdersOutcome {
            user_state: updated_user_state,
        } = compute_cancel_all_orders_outcome(
            &mut ctx.storage,
            USER,
            &user_state,
            Some(&mut events),
            ReasonForOrderRemoval::Liquidated,
        )
        .unwrap();
        user_state = updated_user_state;

        // The user's ask should be gone from the book.
        let user_asks: Vec<_> = ASKS
            .idx
            .user
            .prefix(USER)
            .range(&ctx.storage, None, None, IterationOrder::Ascending)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            user_asks.is_empty(),
            "user's ask should have been cancelled"
        );
        assert_eq!(user_state.open_order_count, 0);

        // Step 2: Run _liquidate (same as the public function does next).
        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(47_500));

        let state = STATE.load(&ctx.storage).unwrap();

        let result = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        );

        assert!(result.is_ok(), "liquidation failed: {:?}", result.err());

        let LiquidateOutcome {
            user_state,
            maker_states,
            ..
        } = result.unwrap();

        // User's position should be fully closed against MAKER's bid.
        assert!(
            user_state.positions.is_empty(),
            "user should have no positions after liquidation"
        );

        // MAKER should now hold a position (absorbed the user's long).
        assert!(
            maker_states[&MAKER].positions.contains_key(&pair_btc()),
            "maker should have a position after absorbing liquidation"
        );
    }

    /// Maker's resting bid has TP/SL child orders. A liquidation matches
    /// against it. After liquidation, the maker's new position should have
    /// the child orders applied and NEXT_ORDER_ID should be incremented.
    #[test]
    fn maker_child_orders_applied_during_liquidation() {
        let mut ctx = MockContext::new()
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        let param = default_param();
        let pair_state = PairState {
            long_oi: Quantity::new_int(10),
            ..Default::default()
        };

        setup_storage(
            &mut ctx.storage,
            &param,
            &[(pair_btc(), btc_pair_param(), pair_state.clone())],
        );

        // USER has long 10 BTC at entry $50k. Oracle drops to $47,500.
        // Equity < maintenance margin → liquidatable.
        save_position(&mut ctx.storage, USER, &pair_btc(), 10, 50_000);

        // MAKER places a bid with TP/SL child orders.
        let maker_state = UserState {
            margin: UsdValue::new_int(100_000),
            open_order_count: 1,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER, &maker_state)
            .unwrap();

        {
            let stored_price = may_invert_price(UsdPrice::new_int(47_500), true);
            let key: OrderKey = (pair_btc(), stored_price, Uint64::new(1));
            let order = LimitOrder {
                user: MAKER,
                size: Quantity::new_int(10),
                reduce_only: false,
                reserved_margin: UsdValue::ZERO,
                created_at: Timestamp::ZERO,
                tp: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(55_000),
                    max_slippage: Dimensionless::new_percent(1),
                    size: None,
                }),
                sl: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(40_000),
                    max_slippage: Dimensionless::new_percent(2),
                    size: None,
                }),
                client_order_id: None,
            };
            BIDS.save(&mut ctx.storage, key, &order).unwrap();
        }

        let mut pair_params = BTreeMap::new();
        pair_params.insert(pair_btc(), btc_pair_param());

        let mut pair_states = BTreeMap::new();
        pair_states.insert(pair_btc(), pair_state);

        let mut oracle_prices = BTreeMap::new();
        oracle_prices.insert(pair_btc(), UsdPrice::new_int(47_500));

        let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        user_state.margin = UsdValue::new_int(2_400);

        let state = STATE.load(&ctx.storage).unwrap();

        let LiquidateOutcome {
            maker_states,
            next_order_id,
            ..
        } = _liquidate(
            &ctx.storage,
            USER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_params,
            &pair_states,
            &user_state,
            &oracle_prices,
            &mut EventBuilder::new(),
        )
        .expect("liquidation should succeed");

        // Maker should have a long position with TP/SL applied.
        let maker = &maker_states[&MAKER];
        let pos = maker
            .positions
            .get(&pair_btc())
            .expect("maker should have a position");

        assert!(pos.size.is_positive(), "maker should be long");

        // TP → Above for long.
        let above = pos
            .conditional_order_above
            .as_ref()
            .expect("TP should be set");
        assert_eq!(above.trigger_price, UsdPrice::new_int(55_000));

        // SL → Below for long.
        let below = pos
            .conditional_order_below
            .as_ref()
            .expect("SL should be set");
        assert_eq!(below.trigger_price, UsdPrice::new_int(40_000));

        // NEXT_ORDER_ID: started at 1, +1 for TP child, +1 for SL child = 3.
        assert_eq!(next_order_id, OrderId::new(3));
    }

    /// With a 10% liquidation buffer, the close schedule should close more of
    /// the position than with zero buffer, leaving post-liquidation equity
    /// above the remaining maintenance margin.
    ///
    /// Setup: long 10 BTC @ $50,000, margin $2,400, oracle $48,000.
    /// - equity = $2,400 + 10*($48,000-$50,000) = $2,400 - $20,000 = -$17,600
    /// - MM = 10 * $48,000 * 5% = $24,000
    /// - Without buffer: deficit = $24,000 - (-$17,600) = $41,600  (> MM → full close)
    /// - With 10% buffer: deficit = $24,000 - (-$17,600)/1.1 = $24,000 + $16,000 = $40,000 (> MM → full close)
    ///
    /// Both cases fully close, so we use a scenario where the difference matters:
    /// long 10 BTC @ $50,000, margin $25,000, oracle $48,000.
    /// - equity = $25,000 + 10*($48,000-$50,000) = $5,000
    /// - MM = 10 * $48,000 * 5% = $24,000
    /// - Without buffer: deficit = $24,000 - $5,000 = $19,000
    ///   close_amount = $19,000 / ($48,000 * 0.05) = $19,000 / $2,400 = 7.916..
    /// - With 10% buffer: deficit = $24,000 - $5,000/1.1 = $24,000 - $4,545.45 = $19,454.54
    ///   close_amount = $19,454.54 / $2,400 = 8.106..
    ///
    /// The buffer version closes ~0.19 BTC more.
    #[test]
    fn liquidation_buffer_closes_more() {
        // --- Run WITHOUT buffer ---
        let remaining_without_buffer = {
            let mut ctx = MockContext::new()
                .with_contract(CONTRACT)
                .with_funds(Coins::default());

            let param = default_param(); // buffer defaults to 0
            let pair_state = PairState {
                long_oi: Quantity::new_int(10),
                index_price: UsdPrice::new_int(48_000),
                ..Default::default()
            };

            setup_storage(
                &mut ctx.storage,
                &param,
                &[(pair_btc(), btc_pair_param(), pair_state.clone())],
            );

            save_position(&mut ctx.storage, USER, &pair_btc(), 10, 50_000);

            let maker_state = UserState {
                margin: UsdValue::new_int(500_000),
                open_order_count: 1,
                ..Default::default()
            };
            USER_STATES
                .save(&mut ctx.storage, MAKER, &maker_state)
                .unwrap();
            save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 10, 48_000);

            let mut pair_params = BTreeMap::new();
            pair_params.insert(pair_btc(), btc_pair_param());

            let mut pair_states = BTreeMap::new();
            pair_states.insert(pair_btc(), pair_state);

            let mut oracle_prices = BTreeMap::new();
            oracle_prices.insert(pair_btc(), UsdPrice::new_int(48_000));

            let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
            user_state.margin = UsdValue::new_int(25_000);

            let state = STATE.load(&ctx.storage).unwrap();

            let LiquidateOutcome { user_state, .. } = _liquidate(
                &ctx.storage,
                USER,
                CONTRACT,
                Timestamp::ZERO,
                &param,
                &state,
                &pair_params,
                &pair_states,
                &user_state,
                &oracle_prices,
                &mut EventBuilder::new(),
            )
            .expect("no-buffer liq should succeed");

            user_state
                .positions
                .get(&pair_btc())
                .map(|p| p.size)
                .unwrap_or(Quantity::ZERO)
        };

        // --- Run WITH 10% buffer ---
        let remaining_with_buffer = {
            let mut ctx = MockContext::new()
                .with_contract(CONTRACT)
                .with_funds(Coins::default());

            let param = Param {
                liquidation_buffer_ratio: Dimensionless::new_permille(100), // 10%
                ..default_param()
            };
            let pair_state = PairState {
                long_oi: Quantity::new_int(10),
                index_price: UsdPrice::new_int(48_000),
                ..Default::default()
            };

            setup_storage(
                &mut ctx.storage,
                &param,
                &[(pair_btc(), btc_pair_param(), pair_state.clone())],
            );

            save_position(&mut ctx.storage, USER, &pair_btc(), 10, 50_000);

            let maker_state = UserState {
                margin: UsdValue::new_int(500_000),
                open_order_count: 1,
                ..Default::default()
            };
            USER_STATES
                .save(&mut ctx.storage, MAKER, &maker_state)
                .unwrap();
            save_bid(&mut ctx.storage, &pair_btc(), 1, MAKER, 10, 48_000);

            let mut pair_params = BTreeMap::new();
            pair_params.insert(pair_btc(), btc_pair_param());

            let mut pair_states = BTreeMap::new();
            pair_states.insert(pair_btc(), pair_state);

            let mut oracle_prices = BTreeMap::new();
            oracle_prices.insert(pair_btc(), UsdPrice::new_int(48_000));

            let mut user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
            user_state.margin = UsdValue::new_int(25_000);

            let state = STATE.load(&ctx.storage).unwrap();

            let LiquidateOutcome { user_state, .. } = _liquidate(
                &ctx.storage,
                USER,
                CONTRACT,
                Timestamp::ZERO,
                &param,
                &state,
                &pair_params,
                &pair_states,
                &user_state,
                &oracle_prices,
                &mut EventBuilder::new(),
            )
            .expect("buffered liq should succeed");

            user_state
                .positions
                .get(&pair_btc())
                .map(|p| p.size)
                .unwrap_or(Quantity::ZERO)
        };

        // With buffer, more is closed → remaining position is smaller.
        assert!(
            remaining_with_buffer < remaining_without_buffer,
            "buffer should cause more position to be closed: \
             without={remaining_without_buffer}, with={remaining_with_buffer}"
        );
    }
}
