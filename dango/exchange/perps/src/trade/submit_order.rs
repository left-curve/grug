use {
    crate::{
        VOLUME_LOOKBACK,
        core::{
            FillPnl, check_margin, check_oi_constraint, compute_available_margin, compute_notional,
            compute_required_margin, compute_trading_fee, execute_fill,
        },
        position_index::{
            PositionIndexUpdate, apply_position_index_updates, compute_position_diff,
        },
        querier::NoCachePerpQuerier,
        query::query_volume,
        referral::{FeeCommissionsOutcome, apply_fee_commissions},
        state::{FEE_RATE_OVERRIDES, PAIR_PARAMS, PAIR_STATES, PARAM, STATE, USER_STATES},
        trade::{ensure_trading_enabled, resize_reduce_only_orders},
    },
    anyhow::{bail, ensure},
    dango_math::{MathResult, Number, NumberConst},
    dango_order_book::{
        ASKS, BIDS, ChildOrder, ClientOrderId, ConditionalOrder, ConditionalOrderPlaced,
        Dimensionless, FillId, LimitOrder, NEXT_FILL_ID, NEXT_ORDER_ID, OrderId, OrderKind,
        OrderPersisted, OrderRemoved, PairId, Quantity, RawFill, ReasonForOrderRemoval,
        RemovedMaker, TimeInForce, TriggerDirection, UsdPrice, UsdValue, WalkBookOutcome, WalkStep,
        check_minimum_order_size, check_price_band, compute_target_price, decompose_fill,
        decrease_liquidity_depths, flush_volumes, increase_liquidity_depths, may_invert_price,
        validate_slippage, walk_book,
    },
    dango_primitives::{
        Addr, EventBuilder, MutableCtx, Order as IterationOrder, QuerierWrapper, Response,
        StdResult, Storage, Timestamp,
    },
    dango_types::perps::{OrderFilled, PairParam, PairState, Param, State, UserState},
    std::collections::{BTreeMap, btree_map::Entry},
};

pub fn submit_order(
    ctx: MutableCtx,
    pair_id: PairId,
    size: Quantity,
    kind: OrderKind,
    reduce_only: bool,
    tp: Option<ChildOrder>,
    sl: Option<ChildOrder>,
) -> anyhow::Result<Response> {
    ensure_trading_enabled(&PARAM.load(ctx.storage)?)?;

    let mut events = EventBuilder::new();

    _submit_order(
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
    )?;

    // No token transfers — all PnL/fees settled via user_state.margin.
    Ok(Response::new().add_events(events)?)
}

/// Intermediate layer of `submit_order`: takes individual components of
/// `MutableCtx` (so multiple invocations can share the same storage within
/// a `batch_update_orders` loop), loads the contract state, delegates the
/// pure decision-making to [`compute_submit_order_outcome`], then writes
/// the resulting mutations back to storage.
///
/// Events are pushed into the caller-owned `events` builder; the caller
/// assembles the `Response`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn _submit_order(
    storage: &mut dyn Storage,
    querier: QuerierWrapper,
    current_time: Timestamp,
    contract: Addr,
    sender: Addr,
    pair_id: PairId,
    size: Quantity,
    kind: OrderKind,
    reduce_only: bool,
    tp: Option<ChildOrder>,
    sl: Option<ChildOrder>,
    events: &mut EventBuilder,
) -> anyhow::Result<()> {
    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    // ---------------------------- 1. Preparation -----------------------------

    let param = PARAM.load(storage)?;
    let state = STATE.load(storage)?;

    let pair_param = PAIR_PARAMS.load(storage, &pair_id)?;
    let pair_state = PAIR_STATES.load(storage, &pair_id)?;

    let taker_state = USER_STATES.may_load(storage, sender)?.unwrap_or_default();

    let oracle_price = PAIR_STATES.load(storage, &pair_id)?.index_price;

    // --------------------------- 2. Business logic ---------------------------

    let SubmitOrderOutcome {
        state,
        pair_state,
        taker_state,
        mut maker_states,
        order_mutations,
        order_to_store,
        next_order_id,
        next_fill_id,
        index_updates,
        volumes,
        fee_breakdowns,
    } = compute_submit_order_outcome(
        storage,
        sender,
        contract,
        current_time,
        &param,
        &state,
        &pair_id,
        &pair_param,
        &pair_state,
        &taker_state,
        oracle_price,
        size,
        kind,
        reduce_only,
        tp,
        sl,
        events,
    )?;

    // ------------------------ 3. Apply state changes -------------------------

    flush_volumes(storage, current_time, &volumes)?;

    maker_states.insert(sender, taker_state);

    let FeeCommissionsOutcome {
        user_states: updated_maker_states,
    } = apply_fee_commissions(
        storage,
        querier,
        contract,
        current_time,
        &param,
        &maker_states,
        fee_breakdowns,
        &volumes,
        events,
    )?;

    let maker_states = updated_maker_states;

    NEXT_ORDER_ID.save(storage, &next_order_id)?;
    NEXT_FILL_ID.save(storage, &next_fill_id)?;

    STATE.save(storage, &state)?;

    PAIR_STATES.save(storage, &pair_id, &pair_state)?;

    for (addr, user_state) in &maker_states {
        USER_STATES.save(storage, *addr, user_state)?;
    }

    apply_position_index_updates(storage, &index_updates)?;

    let (taker_book, maker_book) = if size.is_positive() {
        (BIDS, ASKS)
    } else {
        (ASKS, BIDS)
    };

    for (stored_price, order_id, mutation, pre_fill_abs_size) in order_mutations {
        let order_key = (pair_id.clone(), stored_price, order_id);

        // The maker is on the opposite side of the taker.
        let maker_is_bid = !size.is_positive();
        let real_price = may_invert_price(stored_price, maker_is_bid);

        // Completely remove the old order's liquidity depth contribution.
        // If the order still has some size remaining, we re-add it.
        // Why don't we simply subtract the delta? To avoid a situation known as
        // notional drift. See `../liquidity_depth.rs`, test function `partial_fill_no_residual_depth`
        // for detail.
        decrease_liquidity_depths(
            storage,
            &pair_id,
            maker_is_bid,
            real_price,
            pre_fill_abs_size,
            &pair_param.bucket_sizes,
        )?;

        match mutation {
            Some(order) => {
                increase_liquidity_depths(
                    storage,
                    &pair_id,
                    maker_is_bid,
                    real_price,
                    order.size.checked_abs()?,
                    &pair_param.bucket_sizes,
                )?;

                maker_book.save(storage, order_key, &order)?;
            },
            None => {
                maker_book.remove(storage, order_key)?;
            },
        }
    }

    // Capture the just-placed reduce-only order's id before `order_to_store` is
    // consumed, so the dynamic re-size below can tell whether the sum-clamp
    // evicted it (in which case the whole placement is rejected).
    let new_reduce_only_order_id = if reduce_only {
        order_to_store.as_ref().map(|(_, order_id, _)| *order_id)
    } else {
        None
    };

    if let Some((stored_price, order_id, order)) = order_to_store {
        let is_bid = size.is_positive();
        let limit_price = may_invert_price(stored_price, is_bid);

        increase_liquidity_depths(
            storage,
            &pair_id,
            is_bid,
            limit_price,
            order.size.checked_abs()?,
            &pair_param.bucket_sizes,
        )?;

        taker_book.save(storage, (pair_id.clone(), stored_price, order_id), &order)?;

        events.push(OrderPersisted {
            order_id,
            pair_id: pair_id.clone(),
            user: sender,
            limit_price,
            size: order.size,
            client_order_id: order.client_order_id,
        })?;
    }

    // ---------- Step 9. Dynamic re-size of reduce-only orders ----------------
    //
    // Every fill in this transaction may have moved a user's position, so each
    // affected user's resting reduce-only orders are re-clamped so their sizes
    // sum to no more than the new position (cancelling those left on the wrong
    // side). The affected users are exactly the keys of `maker_states`:
    // settlement inserts the taker (see `maker_states.insert(sender, ..)`
    // above) and every filled maker into it to persist their state.
    //
    // We iterate `maker_states.keys()` rather than the `index_updates` the
    // matching loop also produced. `compute_position_diff` returns `None`
    // whenever `(entry_price, is_positive())` is unchanged, and a pure reduction
    // changes neither — so a maker shrinking long 10 -> 3 yields no index entry,
    // exactly the case re-sizing exists for. Over-scanning `maker_states` is
    // complete and safe: re-sizing a user whose position did not change is a
    // no-op because the invariant already held for them. Do NOT replace this
    // with the index diff.

    // Re-size the sender first so its just-placed order can be rejected before
    // any other user's state is touched. A reduce-only placement whose
    // sum-clamp leaves the new order no budget evicts it here, which we surface
    // as a rejection (the bail rolls back the whole transaction).
    let removed = resize_reduce_only_orders(storage, sender, &pair_id, events)?;
    if let Some(new_order_id) = new_reduce_only_order_id
        && removed.contains(&new_order_id)
    {
        bail!("reduce-only order would exceed position size");
    }

    for maker in maker_states.keys() {
        if *maker == sender {
            continue;
        }
        resize_reduce_only_orders(storage, *maker, &pair_id, events)?;
    }

    #[cfg(feature = "tracing")]
    {
        tracing::info!(
            user = %sender,
            %pair_id,
            %size,
            "Order submitted"
        );
    }

    #[cfg(feature = "metrics")]
    {
        let pair_label = pair_id.to_string();

        metrics::counter!(
            crate::metrics::LABEL_ORDERS_SUBMITTED,
            "pair_id" => pair_label.clone()
        )
        .increment(1);

        metrics::gauge!(
            crate::metrics::LABEL_OPEN_INTEREST_LONG,
            "pair_id" => pair_label.clone()
        )
        .set(pair_state.long_oi.to_f64());

        metrics::gauge!(
            crate::metrics::LABEL_OPEN_INTEREST_SHORT,
            "pair_id" => pair_label.clone()
        )
        .set(pair_state.short_oi.to_f64());

        metrics::histogram!(
            crate::metrics::LABEL_DURATION_SUBMIT_ORDER,
            "pair_id" => pair_label
        )
        .record(start.elapsed().as_secs_f64());
    }

    Ok(())
}

/// Owned outcome of a `compute_submit_order_outcome` call. Every piece of
/// caller-persistable state that the call may have updated is returned in
/// this struct, so that a failed call can discard everything at once
/// simply by dropping the `Err` variant — no partial mutations can leak
/// into the caller's `&mut` parameters. See
/// [`dango/exchange/perps/purity.md`](../../purity.md) for the full rationale.
#[derive(Debug)]
pub struct SubmitOrderOutcome {
    pub state: State,
    pub pair_state: PairState,
    pub taker_state: UserState,
    pub maker_states: BTreeMap<Addr, UserState>,
    pub order_mutations: Vec<(UsdPrice, OrderId, Option<LimitOrder>, Quantity)>,
    pub order_to_store: Option<(UsdPrice, OrderId, LimitOrder)>,
    pub next_order_id: OrderId,
    pub next_fill_id: FillId,
    pub index_updates: Vec<PositionIndexUpdate>,
    pub volumes: BTreeMap<Addr, UsdValue>,
    pub fee_breakdowns: BTreeMap<Addr, FeeBreakdown>,
}

/// Pure order submission: reads from storage but does not write.
/// Takes the dense state structs (`State`, `PairState`, `UserState`) by
/// shared reference, clones them at entry, and returns the updated
/// copies in [`SubmitOrderOutcome`] alongside the deferred order-book
/// mutations and PnL/fee accumulators. A failed call leaves the
/// caller's inputs untouched — see `dango/exchange/perps/purity.md`.
///
/// `apply_fee_commissions` is still called by the entry point on the
/// returned `maker_states`; moving it inside is left as a follow-up
/// since that requires lifting `&mut Storage` and `QuerierWrapper`
/// into the signature and re-plumbing every test call site.
pub(crate) fn compute_submit_order_outcome(
    storage: &dyn Storage,
    taker: Addr,
    contract: Addr,
    current_time: Timestamp,
    param: &Param,
    state: &State,
    pair_id: &PairId,
    pair_param: &PairParam,
    pair_state: &PairState,
    taker_state: &UserState,
    oracle_price: UsdPrice,
    size: Quantity,
    kind: OrderKind,
    reduce_only: bool,
    tp: Option<ChildOrder>,
    sl: Option<ChildOrder>,
    events: &mut EventBuilder,
) -> anyhow::Result<SubmitOrderOutcome> {
    // Clone at entry and mutate locals freely. `events` is the one
    // deliberate `&mut` on caller state per the purity rule exception.
    let mut state = state.clone();
    let mut pair_state = pair_state.clone();
    let mut taker_state = taker_state.clone();

    // `oracle_price` is the mark (`index_price`): used for fills, fees, and
    // minimum-order-size. The order price band instead references the last
    // external oracle price, which does not drift during closed sessions, so
    // resting orders cannot walk the band away from the true price.
    let reference_price = pair_state.oracle_price;

    // -------------- Step 0. Validate prices and slippage --------------------

    match &kind {
        OrderKind::Market { max_slippage } => {
            validate_slippage(*max_slippage, pair_param.max_market_slippage)?;
        },
        OrderKind::Limit { limit_price, .. } => {
            // `check_price_band` subsumes the positivity check: any
            // `max_limit_price_deviation < 1` (enforced at configure time)
            // makes the lower bound strictly positive, so zero or negative
            // `limit_price` is rejected here too.
            check_price_band(
                *limit_price,
                reference_price,
                pair_param.max_limit_price_deviation,
            )?;
        },
    }

    // IOC limit orders never enter the book, so a `client_order_id` would
    // never be reachable for cancellation — disallow it to surface the
    // misconfiguration loudly instead of silently dropping the id.
    if let OrderKind::Limit {
        time_in_force: TimeInForce::ImmediateOrCancel,
        client_order_id: Some(_),
        ..
    } = &kind
    {
        bail!("client_order_id is not allowed with TimeInForce::ImmediateOrCancel");
    }

    for child_order in [&tp, &sl].into_iter().flatten() {
        ensure!(
            child_order.trigger_price.is_positive(),
            "trigger price must be positive: {}",
            child_order.trigger_price
        );

        validate_slippage(child_order.max_slippage, pair_param.max_market_slippage)?;
    }

    // -------------- Step 1. Check minimum order size -------------------------

    if !reduce_only {
        check_minimum_order_size(size, oracle_price, pair_param.min_order_size)?;
    }

    // ----------------------- Step 2. Decompose order -------------------------

    let (closing_size, mut opening_size) = {
        let current_position = taker_state
            .positions
            .get(pair_id)
            .map(|p| p.size)
            .unwrap_or_default();
        decompose_fill(size, current_position)
    };

    if reduce_only {
        opening_size = Quantity::ZERO;
    }

    let fillable_size = closing_size.checked_add(opening_size)?;

    ensure!(fillable_size.is_non_zero(), "fillable size is zero");

    // -------------- Step 3. Check OI constraint for opening ------------------

    check_oi_constraint(opening_size, &pair_state, pair_param)?;

    // --------------- Step 3½. Allocate a unique order ID ---------------------

    let taker_order_id = NEXT_ORDER_ID.load(storage)?;
    let mut next_order_id = taker_order_id + OrderId::ONE;
    let mut next_fill_id = NEXT_FILL_ID.load(storage)?;

    // ---------------------- Step 4. Post-only fast path ----------------------

    if let OrderKind::Limit {
        limit_price,
        time_in_force: TimeInForce::PostOnly,
        client_order_id,
    } = kind
    {
        let StoreLimitOrderOutcome {
            user_state: updated_taker_state,
            stored_price,
            order_id,
            order,
        } = store_post_only_limit_order(
            storage,
            taker,
            current_time,
            param,
            pair_id,
            pair_param,
            &taker_state,
            fillable_size,
            limit_price,
            reduce_only,
            taker_order_id,
            tp,
            sl,
            client_order_id,
        )?;

        taker_state = updated_taker_state;

        return Ok(SubmitOrderOutcome {
            state,
            pair_state,
            taker_state,
            maker_states: BTreeMap::new(),
            order_mutations: Vec::new(),
            order_to_store: Some((stored_price, order_id, order)),
            next_order_id: taker_order_id + OrderId::ONE,
            // Post-only orders cannot match, so no fill id is allocated.
            next_fill_id,
            index_updates: Vec::new(),
            volumes: BTreeMap::new(),
            fee_breakdowns: BTreeMap::new(),
        });
    }

    // ----------------- Step 5: Pre-match taker margin check ------------------
    //
    // Reduce-only orders only reduce exposure, so they skip the check.

    // Determine the taker's fee rate.
    // If the admin has configured a fee rate override for the taker, then
    // simply use it.
    // Otherwise, resolve it based on recent volume.
    let taker_fee_rate = if let Some((_maker_rate_override, taker_rate_override)) =
        FEE_RATE_OVERRIDES.may_load(storage, taker)?
    {
        taker_rate_override
    } else {
        let volume_since = Some(current_time.saturating_sub(VOLUME_LOOKBACK));
        let taker_volume = query_volume(storage, taker, volume_since)?;
        param.taker_fee_rates.resolve(taker_volume)
    };

    if !reduce_only {
        let perp_querier = NoCachePerpQuerier::new_local(storage);

        check_margin(
            pair_id,
            &perp_querier,
            &taker_state,
            taker_fee_rate,
            oracle_price,
            size,
        )?;
    }

    // --------------------- Step 6. Compute target price ----------------------

    let taker_is_bid = size.is_positive();
    let target_price = compute_target_price(kind, oracle_price, taker_is_bid)?;

    // ---------------------- Step 7. Match against book -----------------------

    let MatchOrderOutcome {
        state: updated_state,
        pair_state: updated_pair_state,
        taker_state: updated_taker_state,
        maker_states,
        unfilled,
        volumes,
        fee_breakdowns,
        order_mutations,
        index_updates,
        next_order_id: updated_next_order_id,
        next_fill_id: updated_next_fill_id,
    } = match_order(
        storage,
        taker,
        contract,
        current_time,
        param,
        &state,
        pair_id,
        &pair_state,
        &taker_state,
        taker_is_bid,
        taker_order_id,
        // Surface the taker's `client_order_id` (if any) on the
        // `OrderFilled` event so off-chain consumers can correlate fills
        // with the originally-submitted order.
        match kind {
            OrderKind::Limit {
                client_order_id, ..
            } => client_order_id,
            OrderKind::Market { .. } => None,
        },
        taker_fee_rate,
        None, // no forced maker fee; respect per-user overrides and tier schedule
        &BTreeMap::new(),
        target_price,
        reference_price,
        pair_param.max_limit_price_deviation,
        fillable_size,
        next_order_id,
        next_fill_id,
        events,
    )?;

    state = updated_state;
    pair_state = updated_pair_state;
    taker_state = updated_taker_state;
    next_order_id = updated_next_order_id;
    next_fill_id = updated_next_fill_id;

    // ------------------- Step 8. Handle unfilled remainder -------------------

    let order_to_store = if unfilled.is_non_zero() {
        match kind {
            OrderKind::Market { .. }
            | OrderKind::Limit {
                time_in_force: TimeInForce::ImmediateOrCancel,
                ..
            } => {
                // IOC: discard unfilled remainder, same as market orders.
                ensure!(
                    unfilled.checked_abs()? < fillable_size.checked_abs()?,
                    "no liquidity at acceptable price! target_price: {target_price}"
                );

                None
            },
            OrderKind::Limit {
                limit_price,
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id,
            } => {
                let StoreLimitOrderOutcome {
                    user_state: updated_taker_state,
                    stored_price,
                    order_id,
                    order,
                } = store_limit_order(
                    storage,
                    taker,
                    current_time,
                    param,
                    pair_param,
                    &taker_state,
                    unfilled,
                    limit_price,
                    reduce_only,
                    taker_order_id,
                    tp.clone(),
                    sl.clone(),
                    client_order_id,
                )?;

                taker_state = updated_taker_state;

                Some((stored_price, order_id, order))
            },
            // PostOnly is intercepted at Step 4 and never reaches here.
            OrderKind::Limit {
                time_in_force: TimeInForce::PostOnly,
                ..
            } => unreachable!("post-only handled in Step 4"),
        }
    } else {
        None
    };

    // ---------- Step 8½. Apply taker's child orders after fills --------------

    // If the taker's order has TP/SL setting attached, apply them if:
    // - the taker order was at least partially filled;
    // - the taker now has a position of non-zero size;
    // - the position is of the same direction as the order.

    let had_fills = unfilled != fillable_size;

    if had_fills
        && (tp.is_some() || sl.is_some())
        && let Some(position) = taker_state.positions.get_mut(pair_id)
        && position.size.is_positive() == size.is_positive()
    {
        let (above, below) = map_child_orders(position.size, &tp, &sl, &mut next_order_id);

        position.conditional_order_above = above;
        position.conditional_order_below = below;

        emit_child_order_events(events, pair_id, taker, &tp, &sl, position.size)?;
    }

    // `match_order` has already settled fees and PnLs per-fill and
    // accumulated `fee_breakdowns`; all that's left is to hand them back
    // to the caller for `apply_fee_commissions`.

    Ok(SubmitOrderOutcome {
        state,
        pair_state,
        taker_state,
        maker_states,
        order_mutations,
        order_to_store,
        next_order_id,
        next_fill_id,
        index_updates,
        volumes,
        fee_breakdowns,
    })
}

/// Owned outcome of a `match_order` call. Carries post-match copies of
/// `state`, `pair_state`, `taker_state`, and `maker_states`, plus the
/// per-user accumulators (`volumes`, `fee_breakdowns`) that the caller
/// feeds into `apply_fee_commissions` / `flush_volumes` or merges into
/// its own running totals (`execute_close_schedule`).
///
/// PnL and fee settlement happens per-fill *inside* `match_order` via
/// `settle_pnls`, so they do not appear as accumulators here.
#[derive(Debug)]
pub struct MatchOrderOutcome {
    pub state: State,
    pub pair_state: PairState,
    pub taker_state: UserState,
    pub maker_states: BTreeMap<Addr, UserState>,
    pub unfilled: Quantity,
    pub volumes: BTreeMap<Addr, UsdValue>,
    pub fee_breakdowns: BTreeMap<Addr, FeeBreakdown>,
    pub order_mutations: Vec<(UsdPrice, OrderId, Option<LimitOrder>, Quantity)>,
    pub index_updates: Vec<PositionIndexUpdate>,
    pub next_order_id: OrderId,
    pub next_fill_id: FillId,
}

/// Iterate the opposite-side order book in price-time priority and match
/// the taker against resting orders, accumulating fills, self-trade
/// cancellations, and the associated PnL / fee / volume / position-index
/// deltas. Pure w.r.t. the caller's dense state: takes `&PairState`,
/// `&UserState`, `&BTreeMap<Addr, UserState>`, and an owned
/// `next_order_id`; clones each at entry and returns the updated copies
/// in [`MatchOrderOutcome`]. A failed call drops the locals and leaves
/// the caller's inputs untouched — see `dango/exchange/perps/purity.md`.
///
/// Self-trade prevention (EXPIRE_MAKER): if a resting order belongs to
/// the taker, the order is cancelled and the taker continues matching
/// deeper in the book. This is consistent with Binance's EXPIRE_MAKER
/// mode: <https://developers.binance.com/docs/binance-spot-api-docs/faqs/stp_faq>
pub fn match_order(
    storage: &dyn Storage,
    taker: Addr,
    contract: Addr,
    current_time: Timestamp,
    param: &Param,
    state: &State,
    pair_id: &PairId,
    pair_state: &PairState,
    taker_state: &UserState,
    taker_is_bid: bool,
    taker_order_id: OrderId,
    taker_client_order_id: Option<ClientOrderId>,
    taker_fee_rate: Dimensionless,
    force_maker_fee_rate: Option<Dimensionless>,
    maker_states: &BTreeMap<Addr, UserState>,
    target_price: UsdPrice,
    oracle_price: UsdPrice,
    max_limit_price_deviation: Dimensionless,
    remaining_size: Quantity,
    mut next_order_id: OrderId,
    mut next_fill_id: FillId,
    events: &mut EventBuilder,
) -> anyhow::Result<MatchOrderOutcome> {
    // Clone at entry and mutate locals freely. `events` is the one
    // deliberate `&mut` on caller state per the purity rule exception.
    let mut state = state.clone();
    let mut pair_state = pair_state.clone();
    let mut taker_state = taker_state.clone();
    let mut maker_states = maker_states.clone();

    // Ensure the vault's UserState is in `maker_states` so the per-fill
    // `settle_pnls` below can credit the vault its fee cut when the maker
    // is not the vault. Skip this when the **taker** is the vault — in
    // that case `taker_state` IS the vault's state, and duplicating it
    // into `maker_states` would (a) violate the "taker ∉ maker_states"
    // invariant and (b) silently shadow the taker's updates.
    if taker != contract {
        maker_states.entry(contract).or_insert_with(|| {
            USER_STATES
                .may_load(storage, contract)
                .unwrap()
                .unwrap_or_default()
        });
    }

    let mut volumes: BTreeMap<Addr, UsdValue> = BTreeMap::new();
    let mut fee_breakdowns: BTreeMap<Addr, FeeBreakdown> = BTreeMap::new();
    let mut order_mutations = Vec::new();
    let mut index_updates = Vec::new();

    // Walk the book in price-time priority. The walker is generic over
    // perp settlement: it returns `WalkStep`s as data, and we apply
    // perp-specific mutations (margin release, position update, fee /
    // PnL settlement, OI bookkeeping, OrderFilled / OrderRemoved
    // events) in the same chronological order, matching the legacy
    // interleaved engine's event sequence byte-for-byte.
    //
    // Reduce-only maker orders must never flip the maker's position. The
    // walker re-clamps each reduce-only maker fill against the maker's current
    // position; give it a lookup that mirrors the maker-state source used by
    // the settlement loop below (`maker_states` first, then `USER_STATES`,
    // default zero) so the walk's simulation matches what `settle_fill` sees.
    let maker_position = |addr: Addr| -> StdResult<Quantity> {
        if let Some(s) = maker_states.get(&addr) {
            Ok(s.positions.get(pair_id).map(|p| p.size).unwrap_or_default())
        } else {
            Ok(USER_STATES
                .may_load(storage, addr)?
                .map(|s| s.positions.get(pair_id).map(|p| p.size).unwrap_or_default())
                .unwrap_or_default())
        }
    };

    // Running remainder of the taker's order, decremented on each fill below
    // so every taker-side `OrderFilled` can report the order's post-fill size.
    // Mirrors `walk_book`'s own `remaining_size` decrement using the same
    // per-fill sizes, so the two stay exactly in sync. Captured here before
    // the `remaining_size` binding is shadowed by the walk's leftover.
    let mut taker_remaining_size = remaining_size;

    let WalkBookOutcome {
        steps,
        remaining_size,
    } = walk_book(
        storage,
        pair_id,
        taker,
        contract,
        taker_is_bid,
        target_price,
        oracle_price,
        max_limit_price_deviation,
        remaining_size,
        maker_position,
    )?;

    for step in steps {
        match step {
            // ---------------------- Walked-past removal ----------------------
            //
            // Self-trade prevention or out-of-band cancellation. Update the
            // affected user's state (release reserved margin, decrement
            // `open_order_count`), record the storage mutation as an
            // unconditional removal, and push `OrderRemoved` with the reason
            // the walker supplied so STP and out-of-band cancels stay
            // distinguishable in the event stream.
            WalkStep::Removed(RemovedMaker {
                maker_order_id,
                maker_addr,
                maker_client_order_id,
                maker_pre_fill_size,
                maker_stored_price,
                maker_real_price: _,
                maker_order,
                reason,
            }) => {
                // STP touches the taker's own state; out-of-band cancels
                // touch the maker's state (loaded from `maker_states` /
                // `USER_STATES` if absent).
                if maker_addr == taker {
                    taker_state.open_order_count -= 1;
                    (taker_state.reserved_margin)
                        .checked_sub_assign(maker_order.reserved_margin)?;
                } else {
                    let maker_state = match maker_states.entry(maker_addr) {
                        Entry::Vacant(e) => {
                            let s = USER_STATES
                                .may_load(storage, maker_addr)?
                                .unwrap_or_default();
                            e.insert(s)
                        },
                        Entry::Occupied(e) => e.into_mut(),
                    };

                    maker_state.open_order_count -= 1;
                    maker_state
                        .reserved_margin
                        .checked_sub_assign(maker_order.reserved_margin)?;
                }

                order_mutations.push((
                    maker_stored_price,
                    maker_order_id,
                    None,
                    maker_pre_fill_size,
                ));

                events.push(OrderRemoved {
                    order_id: maker_order_id,
                    pair_id: pair_id.clone(),
                    user: maker_addr,
                    reason,
                    client_order_id: maker_client_order_id,
                })?;
            },

            // ---------------------- Settle one fill --------------------------
            WalkStep::Fill(RawFill {
                maker_order_id,
                maker_addr: maker_user,
                maker_client_order_id,
                maker_pre_fill_size,
                maker_post_fill_size,
                maker_stored_price,
                fill_price,
                fill_size: taker_fill_size,
                maker_order,
            }) => {
                let maker_fill_size = taker_fill_size.checked_neg()?;

                // Post-fill order remainders reported on the two `OrderFilled`
                // events below. Decrement the taker's running remainder (the
                // same trajectory `walk_book` computed); the maker's signed
                // remainder is its pre-fill size less what it just filled, and
                // is zero on a full fill.
                taker_remaining_size.checked_sub_assign(taker_fill_size)?;
                let maker_remaining_size = maker_order.size.checked_sub(maker_fill_size)?;

                // ---------------- Allocate a shared fill id ------------------
                //
                // Both `OrderFilled` events below carry this `fill_id`, so
                // downstream consumers can group the two sides of the match.
                let fill_id = next_fill_id;
                next_fill_id = next_fill_id.checked_add(FillId::ONE)?;

                // -------------------- Settle taker side ----------------------

                let old_taker_pos = taker_state.positions.get(pair_id).cloned();

                let taker_settlement = settle_fill(
                    contract,
                    pair_id,
                    &mut pair_state,
                    &mut taker_state,
                    taker,
                    taker_fill_size,
                    fill_price,
                    taker_fee_rate,
                    Some((
                        events,
                        taker_order_id,
                        taker_client_order_id,
                        fill_id,
                        false,
                        taker_remaining_size,
                    )),
                )?;

                volumes
                    .entry(taker)
                    .or_default()
                    .checked_add_assign(taker_settlement.volume)?;

                if let Some(diff) = compute_position_diff(
                    pair_id,
                    taker,
                    old_taker_pos.as_ref(),
                    taker_state.positions.get(pair_id),
                ) {
                    index_updates.push(diff);
                }

                // -------------------- Settle maker side ----------------------
                //
                // Determine the maker's fee rate.
                // - If the caller forces a rate (e.g. zero during
                //   liquidation), use it and bypass both the override and
                //   the tier schedule so the zero-fee invariant cannot be
                //   defeated by a pre-existing override.
                // - Else if the admin has configured a fee rate override
                //   for the maker, use it.
                // - Otherwise, resolve it based on recent volume.
                let maker_fee_rate = if let Some(forced) = force_maker_fee_rate {
                    forced
                } else if let Some((maker_rate_override, _taker_rate_override)) =
                    FEE_RATE_OVERRIDES.may_load(storage, maker_user)?
                {
                    maker_rate_override
                } else {
                    let volume_since = Some(current_time.saturating_sub(VOLUME_LOOKBACK));
                    let maker_volume = query_volume(storage, maker_user, volume_since)?;
                    param.maker_fee_rates.resolve(maker_volume)
                };

                // Take the maker's user state out of the map so the later
                // `settle_pnls` call can borrow the vault's state from the
                // map disjointly. We reinsert it at the end of the iteration.
                let mut maker_state = match maker_states.remove(&maker_user) {
                    Some(s) => s,
                    None => USER_STATES
                        .may_load(storage, maker_user)?
                        .unwrap_or_default(),
                };

                let old_maker_pos = maker_state.positions.get(pair_id).cloned();

                let maker_settlement = settle_fill(
                    contract,
                    pair_id,
                    &mut pair_state,
                    &mut maker_state,
                    maker_user,
                    maker_fill_size,
                    fill_price,
                    maker_fee_rate,
                    Some((
                        events,
                        maker_order_id,
                        maker_client_order_id,
                        fill_id,
                        true,
                        maker_remaining_size,
                    )),
                )?;

                volumes
                    .entry(maker_user)
                    .or_default()
                    .checked_add_assign(maker_settlement.volume)?;

                if let Some(diff) = compute_position_diff(
                    pair_id,
                    maker_user,
                    old_maker_pos.as_ref(),
                    maker_state.positions.get(pair_id),
                ) {
                    index_updates.push(diff);
                }

                // ----------------- Per-fill net-fee settlement ---------------

                let fill_breakdowns = {
                    // vault_state_opt carries the vault's state only when
                    // neither side of the fill is the vault itself. When
                    // the taker or maker IS the vault, that party's own
                    // state holds the vault's balance and settle_pnls
                    // routes the vault fee there.
                    let vault_state_opt = if taker != contract && maker_user != contract {
                        Some(
                            maker_states
                                .get_mut(&contract)
                                .expect("vault inserted at match_order entry"),
                        )
                    } else {
                        None
                    };
                    settle_pnls(
                        contract,
                        param,
                        &mut state,
                        taker,
                        &mut taker_state,
                        taker_settlement.pnl.total()?,
                        taker_settlement.fee,
                        maker_user,
                        &mut maker_state,
                        maker_settlement.pnl.total()?,
                        maker_settlement.fee,
                        vault_state_opt,
                    )?
                };

                if let Some(bd) = fill_breakdowns.taker {
                    merge_fee_breakdown(&mut fee_breakdowns, taker, bd)?;
                }
                if let Some(bd) = fill_breakdowns.maker {
                    merge_fee_breakdown(&mut fee_breakdowns, maker_user, bd)?;
                }

                // ----------- Apply maker's child orders after fill -----------

                if (maker_order.tp.is_some() || maker_order.sl.is_some())
                    && let Some(maker_pos) = maker_state.positions.get_mut(pair_id)
                    && maker_pos.size.is_positive() == maker_order.size.is_positive()
                {
                    let (above, below) = map_child_orders(
                        maker_pos.size,
                        &maker_order.tp,
                        &maker_order.sl,
                        &mut next_order_id,
                    );

                    maker_pos.conditional_order_above = above;
                    maker_pos.conditional_order_below = below;

                    emit_child_order_events(
                        events,
                        pair_id,
                        maker_user,
                        &maker_order.tp,
                        &maker_order.sl,
                        maker_pos.size,
                    )?;
                }

                // ------------ Update maker's order and user state ------------
                //
                // Release reserved margin. On a full fill, release
                // everything that's left in the order: the proportional
                // formula truncates toward zero and would otherwise
                // orphan the residual in `maker_state` when the order is
                // removed from storage a few lines below.
                let margin_to_release = if maker_post_fill_size.is_zero() {
                    maker_order.reserved_margin
                } else {
                    (maker_order.reserved_margin)
                        .checked_mul(maker_fill_size)?
                        .checked_div(maker_order.size)?
                };

                maker_state
                    .reserved_margin
                    .checked_sub_assign(margin_to_release)?;

                if maker_post_fill_size.is_zero() {
                    maker_state.open_order_count -= 1;

                    order_mutations.push((
                        maker_stored_price,
                        maker_order_id,
                        None,
                        maker_pre_fill_size,
                    ));

                    // Vault order removal is internal churn — suppress the event.
                    if maker_user != contract {
                        events.push(OrderRemoved {
                            order_id: maker_order_id,
                            pair_id: pair_id.clone(),
                            user: maker_user,
                            reason: ReasonForOrderRemoval::Filled,
                            client_order_id: maker_client_order_id,
                        })?;
                    }

                    #[cfg(feature = "metrics")]
                    {
                        metrics::counter!(
                            crate::metrics::LABEL_ORDERS_FILLED,
                            "pair_id" => pair_id.to_string()
                        )
                        .increment(1);
                    }
                } else {
                    // Build the post-fill maker order: same fields as the
                    // pre-fill snapshot, with `size` and `reserved_margin`
                    // decremented by the fill's share. The walker handed
                    // back the original order by value, so we mutate the
                    // local copy to avoid an unnecessary clone.
                    let mut updated_maker_order = maker_order;
                    updated_maker_order
                        .reserved_margin
                        .checked_sub_assign(margin_to_release)?;
                    updated_maker_order.size =
                        updated_maker_order.size.checked_sub(maker_fill_size)?;

                    order_mutations.push((
                        maker_stored_price,
                        maker_order_id,
                        Some(updated_maker_order),
                        maker_pre_fill_size,
                    ));
                }

                // Reinsert the maker's state now that we no longer need
                // disjoint access to the vault state.
                maker_states.insert(maker_user, maker_state);
            },
        }
    }

    Ok(MatchOrderOutcome {
        state,
        pair_state,
        taker_state,
        maker_states,
        unfilled: remaining_size,
        volumes,
        fee_breakdowns,
        order_mutations,
        index_updates,
        next_order_id,
        next_fill_id,
    })
}

/// Per-fill outcome returned by [`settle_fill`]. Callers typically feed
/// `pnl.total()?` and `fee` directly into [`settle_pnls`], and accumulate
/// `volume` across all of a taker order's fills for `apply_fee_commissions`
/// and `flush_volumes`.
///
/// `pnl` is the [`FillPnl`] split into its `funding` and `closing`
/// components so callers that report them separately — e.g. the ADL path
/// emitting `Liquidated.adl_realized_funding` — can read each without
/// arithmetic.
#[derive(Debug, Clone, Copy)]
pub struct FillSettlement {
    pub pnl: FillPnl,
    pub fee: UsdValue,
    pub volume: UsdValue,
}

/// Mutates:
///
/// - `pair_state.long_oi` / `pair_state.short_oi` — updated by `execute_fill`.
/// - `user_state.positions` — opened / closed / flipped by `execute_fill`.
/// - `events` — `OrderFilled` event pushed (if `Some`).
///
/// Returns: per-fill `pnl`, `fee`, and `volume` for `user`. The caller
/// applies them to margins (via `settle_pnls`) and to any running volume
/// accumulator it maintains.
pub fn settle_fill(
    contract: Addr,
    pair_id: &PairId,
    pair_state: &mut PairState,
    user_state: &mut UserState,
    user: Addr,
    fill_size: Quantity,
    fill_price: UsdPrice,
    fee_rate: Dimensionless,
    events: Option<(
        &mut EventBuilder,
        OrderId,
        Option<ClientOrderId>,
        FillId,
        bool,
        // The order's remaining unfilled size after this fill, forwarded to
        // `OrderFilled.remaining_order_size`.
        Quantity,
    )>,
) -> StdResult<FillSettlement> {
    let (closing, opening) = {
        let current_pos = user_state
            .positions
            .get(pair_id)
            .map(|p| p.size)
            .unwrap_or_default();
        decompose_fill(fill_size, current_pos)
    };

    let pnl = execute_fill(
        pair_id, pair_state, user_state, fill_price, closing, opening,
    )?;

    // The vault is exempt from trading fees.
    let fee = if user != contract {
        compute_trading_fee(fill_size, fill_price, fee_rate)?
    } else {
        UsdValue::ZERO
    };

    let volume = compute_notional(fill_size, fill_price)?;

    if let Some((events, order_id, client_order_id, fill_id, is_maker, remaining_order_size)) =
        events
    {
        // `execute_fill` just mutated the position; read its resulting size.
        // An absent entry means the fill closed the position entirely, which
        // `unwrap_or_default` reports as zero.
        let remaining_position_size = user_state
            .positions
            .get(pair_id)
            .map(|p| p.size)
            .unwrap_or_default();

        events.push(OrderFilled {
            order_id,
            pair_id: pair_id.clone(),
            user,
            fill_price,
            fill_size,
            closing_size: closing,
            opening_size: opening,
            realized_pnl: pnl.closing,
            realized_funding: Some(pnl.funding),
            fee,
            client_order_id,
            fill_id: Some(fill_id),
            is_maker: Some(is_maker),
            remaining_order_size: Some(remaining_order_size),
            remaining_position_size: Some(remaining_position_size),
        })?;
    }

    #[cfg(feature = "metrics")]
    {
        let pair_label = pair_id.to_string();

        metrics::counter!(
            crate::metrics::LABEL_TRADES,
            "pair_id" => pair_label.clone()
        )
        .increment(1);

        let vol = volume.to_f64().abs();

        metrics::histogram!(
            crate::metrics::LABEL_VOLUME_PER_TRADE,
            "pair_id" => pair_label.clone()
        )
        .record(vol);

        metrics::histogram!(
            crate::metrics::LABEL_FEES_COLLECTED,
            "pair_id" => pair_label
        )
        .record(fee.to_f64().abs());
    }

    Ok(FillSettlement { pnl, fee, volume })
}

#[derive(Debug, Clone, Copy)]
pub struct FeeBreakdown {
    /// Portion of the fee routed to the protocol treasury.
    pub protocol_fee: UsdValue,

    /// Portion of the fee credited to the vault.
    pub vault_fee: UsdValue,
}

impl FeeBreakdown {
    pub const ZERO: Self = Self {
        protocol_fee: UsdValue::ZERO,
        vault_fee: UsdValue::ZERO,
    };
}

/// Per-fill settlement outcome: the protocol/vault split attributable to
/// each party. Either side is `None` if that party owed no fee on this fill
/// (e.g. the vault when it is the maker — vaults are fee-exempt).
///
/// Rebaters (negative fee) land as `Some(FeeBreakdown { ..ZERO })` so that
/// `apply_fee_commissions` still runs their referral volume tracking; their
/// proportional `vault_fee` is zero by construction.
#[derive(Debug, Clone, Copy)]
pub struct FillFeeBreakdowns {
    pub taker: Option<FeeBreakdown>,
    pub maker: Option<FeeBreakdown>,
}

/// Merge a single party's fee breakdown into a running per-user total.
pub fn merge_fee_breakdown(
    map: &mut BTreeMap<Addr, FeeBreakdown>,
    addr: Addr,
    bd: FeeBreakdown,
) -> MathResult<()> {
    let entry = map.entry(addr).or_insert(FeeBreakdown::ZERO);

    entry.protocol_fee.checked_add_assign(bd.protocol_fee)?;
    entry.vault_fee.checked_add_assign(bd.vault_fee)
}

/// Settle one fill's PnLs and fees on the taker's and maker's margins.
///
/// A "fill" is a single trade between **one taker** and **one maker** at a
/// single price. The function implements the net-fee distribution model:
///
/// 1. `net_fee = taker_fee + maker_fee`. The constraint
///    `taker_fee + maker_fee ≥ 0` (enforced at parameter / override set
///    time) ensures the net is non-negative.
/// 2. Treasury takes `net_fee × protocol_fee_rate`.
/// 3. Vault receives the remainder `net_fee × (1 − protocol_fee_rate)`.
/// 4. Each party's contribution to the referrer pool is weighted by
///    `max(fee, 0)`, so a rebating party contributes zero weight (and
///    therefore produces zero referrer commissions downstream).
///
/// When either the taker or the maker *is* the vault (`== contract`), the
/// vault's fee cut is credited to that party's state and `vault_state`
/// must be `None`. When neither party is the vault, `vault_state` must
/// be `Some(&mut <vault's UserState>)`. Self-trade prevention
/// guarantees taker and maker are never both the vault.
///
/// Mutates:
///
/// - `state.treasury` — credited with `protocol_fee`.
/// - `taker_state.margin` — adjusted by `taker_pnl` and `−taker_fee`.
///   When `taker == contract`, also credited with `vault_fee`.
/// - `maker_state.margin` — adjusted by `maker_pnl` and `−maker_fee`.
///   When `maker == contract`, also credited with `vault_fee`.
/// - `vault_state.margin` (when `Some`) — credited with `vault_fee`.
pub fn settle_pnls(
    contract: Addr,
    param: &Param,
    state: &mut State,
    taker: Addr,
    taker_state: &mut UserState,
    taker_pnl: UsdValue,
    taker_fee: UsdValue,
    maker: Addr,
    maker_state: &mut UserState,
    maker_pnl: UsdValue,
    maker_fee: UsdValue,
    vault_state: Option<&mut UserState>,
) -> anyhow::Result<FillFeeBreakdowns> {
    debug_assert!(taker != maker, "self-trade prevention violated");
    debug_assert!(
        (taker == contract || maker == contract) == vault_state.is_none(),
        "vault_state must be None iff one of the parties is the vault"
    );
    debug_assert!(
        taker != contract || taker_fee.is_zero(),
        "vault as taker must be fee-exempt",
    );
    debug_assert!(
        maker != contract || maker_fee.is_zero(),
        "vault as maker must be fee-exempt",
    );

    // Net fee, protocol cut, vault cut.
    let net_fee = taker_fee.checked_add(maker_fee)?;
    let protocol_fee = net_fee.checked_mul(param.protocol_fee_rate)?;
    let vault_fee = net_fee.checked_sub(protocol_fee)?;

    state.treasury.checked_add_assign(protocol_fee)?;

    // Fee-side margin adjustments. Vault is fee-exempt on its own fills.
    if taker != contract && !taker_fee.is_zero() {
        taker_state.margin.checked_sub_assign(taker_fee)?;
    }
    if maker != contract && !maker_fee.is_zero() {
        maker_state.margin.checked_sub_assign(maker_fee)?;
    }

    // Vault receives its cut. Route it to whichever state corresponds
    // to the vault — the separately-passed `vault_state` when neither
    // party is the vault, otherwise the party's own state.
    match vault_state {
        Some(vs) => vs.margin.checked_add_assign(vault_fee)?,
        None if taker == contract => taker_state.margin.checked_add_assign(vault_fee)?,
        None => maker_state.margin.checked_add_assign(vault_fee)?,
    }

    // PnL adjustments.
    if !taker_pnl.is_zero() {
        taker_state.margin.checked_add_assign(taker_pnl)?;
    }
    if !maker_pnl.is_zero() {
        maker_state.margin.checked_add_assign(maker_pnl)?;
    }

    // Per-party FeeBreakdown. When both sides pay positive fees, the taker
    // takes a weighted slice and the maker the exact residual so the
    // breakdowns sum to `protocol_fee` and `vault_fee` precisely. When only
    // one side has a positive contribution, that side takes the full pool
    // directly — no weight-multiply runs, so no rounding can drift onto the
    // rebater.
    let (taker_breakdown, maker_breakdown) =
        match (taker_fee.is_positive(), maker_fee.is_positive()) {
            (true, true) => {
                let weight = taker_fee.checked_div(net_fee)?;
                let taker = FeeBreakdown {
                    protocol_fee: protocol_fee.checked_mul(weight)?,
                    vault_fee: vault_fee.checked_mul(weight)?,
                };
                let maker = FeeBreakdown {
                    protocol_fee: protocol_fee.checked_sub(taker.protocol_fee)?,
                    vault_fee: vault_fee.checked_sub(taker.vault_fee)?,
                };
                (taker, maker)
            },
            (true, false) => (
                FeeBreakdown {
                    protocol_fee,
                    vault_fee,
                },
                FeeBreakdown::ZERO,
            ),
            (false, true) => (
                FeeBreakdown::ZERO,
                FeeBreakdown {
                    protocol_fee,
                    vault_fee,
                },
            ),
            (false, false) => (FeeBreakdown::ZERO, FeeBreakdown::ZERO),
        };

    Ok(FillFeeBreakdowns {
        taker: (taker != contract && !taker_fee.is_zero()).then_some(taker_breakdown),
        maker: (maker != contract && !maker_fee.is_zero()).then_some(maker_breakdown),
    })
}

/// Validate and store a post-only limit order. Rejects if the limit price
/// would cross the best resting order on the opposite side of the book.
/// Pure w.r.t. the caller's `UserState` — delegates to `store_limit_order`
/// and returns the same `StoreLimitOrderOutcome`.
fn store_post_only_limit_order(
    storage: &dyn Storage,
    taker: Addr,
    current_time: Timestamp,
    param: &Param,
    pair_id: &PairId,
    pair_param: &PairParam,
    taker_state: &UserState,
    size: Quantity,
    limit_price: UsdPrice,
    reduce_only: bool,
    order_id: OrderId,
    tp: Option<ChildOrder>,
    sl: Option<ChildOrder>,
    client_order_id: Option<ClientOrderId>,
) -> anyhow::Result<StoreLimitOrderOutcome> {
    let taker_is_bid = size.is_positive();
    let maker_is_bid = !taker_is_bid;

    let maker_book = if taker_is_bid {
        ASKS
    } else {
        BIDS
    };

    if let Some(record) = maker_book
        .prefix(pair_id.clone())
        .range(storage, None, None, IterationOrder::Ascending)
        .next()
    {
        let ((stored_price, _), _) = record?;
        let best_price = may_invert_price(stored_price, maker_is_bid);

        if taker_is_bid {
            ensure!(
                limit_price < best_price,
                "post-only buy at {limit_price} would cross best ask at {best_price}"
            );
        } else {
            ensure!(
                limit_price > best_price,
                "post-only sell at {limit_price} would cross best bid at {best_price}"
            );
        }
    }

    store_limit_order(
        storage,
        taker,
        current_time,
        param,
        pair_param,
        taker_state,
        size,
        limit_price,
        reduce_only,
        order_id,
        tp,
        sl,
        client_order_id,
    )
}

/// Owned outcome of a `store_limit_order` call (and, by extension, of
/// `store_post_only_limit_order`, which delegates). Returned instead of
/// mutating the caller's `&mut UserState` so that a failed store leaves
/// the caller's state untouched.
#[derive(Debug)]
pub struct StoreLimitOrderOutcome {
    pub user_state: UserState,
    pub stored_price: UsdPrice,
    pub order_id: OrderId,
    pub order: LimitOrder,
}

/// Validate the caller, compute the reserved margin, and produce an
/// updated `UserState` with the new resting order accounted for (incremented
/// `open_order_count`, added `reserved_margin`). Pure w.r.t. the caller's
/// `UserState` — takes `&UserState` and returns the updated copy in the
/// outcome, so a failed call leaves the caller's state untouched.
///
/// The caller is responsible for persisting `outcome.user_state` back to
/// `USER_STATES` and writing the returned `order` to `BIDS` / `ASKS`.
fn store_limit_order(
    storage: &dyn Storage,
    user: Addr,
    current_time: Timestamp,
    param: &Param,
    pair_param: &PairParam,
    user_state: &UserState,
    size: Quantity,
    limit_price: UsdPrice,
    reduce_only: bool,
    order_id: OrderId,
    tp: Option<ChildOrder>,
    sl: Option<ChildOrder>,
    client_order_id: Option<ClientOrderId>,
) -> anyhow::Result<StoreLimitOrderOutcome> {
    ensure!(
        user_state.open_order_count < param.max_open_orders,
        "too many open orders! max allowed: {}",
        param.max_open_orders
    );

    // Enforce tick size: `limit_price` must be an integer multiple of `tick_size`.
    if pair_param.tick_size.is_non_zero() {
        ensure!(
            limit_price.checked_rem(pair_param.tick_size)?.is_zero(),
            "limit price ({}) is not a multiple of tick size ({})",
            limit_price,
            pair_param.tick_size,
        );
    }

    // Reserve margin for worst case (entire order is opening).
    let margin_to_reserve = compute_required_margin(size, limit_price, pair_param)?;

    // 0%-fill margin check: verify the user can afford this reservation.
    if !reduce_only {
        let perp_querier = NoCachePerpQuerier::new_local(storage);
        let available_margin = compute_available_margin(&perp_querier, user_state)?;

        ensure!(
            available_margin >= margin_to_reserve,
            "insufficient margin for limit order: available ({}) < required ({})",
            available_margin,
            margin_to_reserve
        );
    }

    // Clone the user state and mutate the local copy. On `Err` the clone is
    // dropped with the rest of the call frame; the caller's `&UserState`
    // is never touched.
    let mut user_state = user_state.clone();
    user_state.open_order_count += 1;
    user_state
        .reserved_margin
        .checked_add_assign(margin_to_reserve)?;

    // Invert price for buy orders so storage order matches price-time priority.
    let stored_price = may_invert_price(limit_price, size.is_positive());

    Ok(StoreLimitOrderOutcome {
        user_state,
        stored_price,
        order_id,
        order: LimitOrder {
            user,
            size,
            reduce_only,
            reserved_margin: margin_to_reserve,
            created_at: current_time,
            tp,
            sl,
            client_order_id,
        },
    })
}

/// Map TP/SL child order params to above/below conditional orders based on
/// position direction, allocating order IDs from the shared counter.
///
/// - Long positions: TP → Above, SL → Below
/// - Short positions: TP → Below, SL → Above
fn map_child_orders(
    position_size: Quantity,
    tp: &Option<ChildOrder>,
    sl: &Option<ChildOrder>,
    next_order_id: &mut OrderId,
) -> (Option<ConditionalOrder>, Option<ConditionalOrder>) {
    let make_conditional = |child: &ChildOrder, next_id: &mut OrderId| -> ConditionalOrder {
        let order_id = *next_id;
        *next_id += OrderId::ONE;

        ConditionalOrder {
            order_id,
            size: child.size,
            trigger_price: child.trigger_price,
            max_slippage: child.max_slippage,
        }
    };

    let is_long = position_size.is_positive();

    // Map TP/SL to above/below based on direction.
    let (above_src, below_src) = if is_long {
        (tp, sl)
    } else {
        (sl, tp)
    };

    let above = above_src
        .as_ref()
        .map(|c| make_conditional(c, next_order_id));
    let below = below_src
        .as_ref()
        .map(|c| make_conditional(c, next_order_id));

    (above, below)
}

/// Emit `ConditionalOrderPlaced` events for child orders that were applied.
fn emit_child_order_events(
    events: &mut EventBuilder,
    pair_id: &PairId,
    user: Addr,
    tp: &Option<ChildOrder>,
    sl: &Option<ChildOrder>,
    position_size: Quantity,
) -> anyhow::Result<()> {
    let is_long = position_size.is_positive();

    if let Some(child) = tp {
        let direction = if is_long {
            TriggerDirection::Above
        } else {
            TriggerDirection::Below
        };

        events.push(ConditionalOrderPlaced {
            pair_id: pair_id.clone(),
            user,
            trigger_price: child.trigger_price,
            trigger_direction: direction,
            size: child.size,
            max_slippage: child.max_slippage,
        })?;
    }

    if let Some(child) = sl {
        let direction = if is_long {
            TriggerDirection::Below
        } else {
            TriggerDirection::Above
        };

        events.push(ConditionalOrderPlaced {
            pair_id: pair_id.clone(),
            user,
            trigger_price: child.trigger_price,
            trigger_direction: direction,
            size: child.size,
            max_slippage: child.max_slippage,
        })?;
    }

    Ok(())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::USER_STATES,
        dango_math::Uint64,
        dango_order_book::{Dimensionless, FundingPerUnit},
        dango_primitives::{Coins, EventName, JsonDeExt, MockContext, ResultExt, Timestamp},
        dango_types::perps::{Position, RateSchedule},
    };

    const CONTRACT: Addr = Addr::mock(0);
    const TAKER: Addr = Addr::mock(1);
    const MAKER_A: Addr = Addr::mock(2);
    const MAKER_B: Addr = Addr::mock(3);

    /// Large collateral value that trivially satisfies any margin check.
    const LARGE_COLLATERAL: UsdValue = UsdValue::new_int(999_999_999);

    fn pair_id() -> PairId {
        "perp/btcusd".parse().unwrap()
    }

    fn test_param() -> Param {
        Param {
            max_open_orders: 10,
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_permille(1), // 0.1%
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn test_pair_param() -> PairParam {
        PairParam {
            max_abs_oi: Quantity::new_int(1_000_000),
            tick_size: UsdPrice::new_int(1),
            initial_margin_ratio: Dimensionless::new_permille(50), // 5%
            maintenance_margin_ratio: Dimensionless::new_permille(25), // 2.5%
            max_limit_price_deviation: Dimensionless::new_permille(500), // 50%
            // 99.9% — permissive cap used by the drift-cancel tests, which
            // need wide slippage so the walk reaches out-of-band makers
            // before `target_price` would break.
            max_market_slippage: Dimensionless::new_permille(999),
            ..Default::default()
        }
    }

    fn setup_storage(storage: &mut dyn Storage) {
        PARAM.save(storage, &test_param()).unwrap();
        PAIR_PARAMS
            .save(storage, &pair_id(), &test_pair_param())
            .unwrap();
        PAIR_STATES
            .save(
                storage,
                &pair_id(),
                &PairState {
                    index_price: UsdPrice::new_percent(5_000_000), // $50,000
                    oracle_price: UsdPrice::new_percent(5_000_000), // $50,000
                    ..Default::default()
                },
            )
            .unwrap();
        NEXT_ORDER_ID.save(storage, &Uint64::new(1)).unwrap();
        NEXT_FILL_ID.save(storage, &FillId::ONE).unwrap();
    }

    /// Place a resting ask (sell) order on the book.
    fn place_ask(storage: &mut dyn Storage, maker: Addr, price: i128, size: i128, order_id: u64) {
        let key = (pair_id(), UsdPrice::new_int(price), Uint64::new(order_id));
        let order = LimitOrder {
            user: maker,
            size: Quantity::new_int(-size.abs()),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(size.abs() * price / 20), // 5% margin
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        ASKS.save(storage, key, &order).unwrap();

        let mut maker_state = USER_STATES
            .may_load(storage, maker)
            .unwrap()
            .unwrap_or_default();
        maker_state.open_order_count += 1;
        maker_state
            .reserved_margin
            .checked_add_assign(order.reserved_margin)
            .unwrap();
        USER_STATES.save(storage, maker, &maker_state).unwrap();
    }

    /// Place a resting bid (buy) order on the book.
    fn place_bid(storage: &mut dyn Storage, maker: Addr, price: i128, size: i128, order_id: u64) {
        let inverted_price = !UsdPrice::new_int(price);
        let key = (pair_id(), inverted_price, Uint64::new(order_id));
        let order = LimitOrder {
            user: maker,
            size: Quantity::new_int(size.abs()),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(size.abs() * price / 20),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        BIDS.save(storage, key, &order).unwrap();

        let mut maker_state = USER_STATES
            .may_load(storage, maker)
            .unwrap()
            .unwrap_or_default();
        maker_state.open_order_count += 1;
        maker_state
            .reserved_margin
            .checked_add_assign(order.reserved_margin)
            .unwrap();
        USER_STATES.save(storage, maker, &maker_state).unwrap();
    }

    // =================== Market buy: single fill, full fill ==================

    #[test]
    fn market_buy_single_full_fill() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state,
            taker_state,
            order_mutations,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100), // 10%
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Taker should have a long position.
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(10));
        assert_eq!(pos.entry_price, UsdPrice::new_int(50_000));

        // No order stored (market order, fully filled).
        assert!(order_to_store.is_none());

        // OI updated.
        assert_eq!(pair_state.long_oi, Quantity::new_int(10));

        // Ask should be removed from book (1 removal mutation).
        assert_eq!(order_mutations.len(), 1);
        assert!(order_mutations[0].2.is_none());
    }

    // ============= Market buy: partial fill (IOC cancels remainder) ===========

    #[test]
    fn market_buy_partial_fill_ioc() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 5, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Taker gets 5 (partial), remainder canceled (IOC).
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(5));

        assert!(order_to_store.is_none());
    }

    // ============= Market buy: no liquidity at acceptable price ==============

    #[test]
    fn market_buy_no_liquidity_errors() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(10),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("no liquidity at acceptable price")
        );
    }

    // ============= Limit buy: fully fills against book =======================

    #[test]
    fn limit_buy_fully_fills() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(10));

        assert!(order_to_store.is_none());
    }

    // ============ Limit buy: partial fill, remainder stored as GTC ============

    #[test]
    fn limit_buy_partial_fill_remainder_stored() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 5, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // 5 filled.
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(5));

        // Remainder (5) stored as GTC.
        assert!(order_to_store.is_some());
        let (_, _, order) = order_to_store.unwrap();
        assert_eq!(order.size, Quantity::new_int(5));
    }

    // ====== Limit buy: no matchable orders, entire order stored ==============

    #[test]
    fn limit_buy_no_match_stored_entirely() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 51_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // No position (nothing filled).
        assert!(!taker_state.positions.contains_key(&pair_id()));

        // Entire order stored.
        assert!(order_to_store.is_some());
        let (_, _, order) = order_to_store.unwrap();
        assert_eq!(order.size, Quantity::new_int(10));
    }

    // ======== Limit buy IOC: partial fill, remainder cancelled ================

    #[test]
    fn limit_buy_ioc_partial_fill_remainder_cancelled() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 5, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            taker_state,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::ImmediateOrCancel,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // 5 filled.
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(5));

        // IOC: remainder discarded, not stored.
        assert!(order_to_store.is_none());

        // No reserved margin or open orders.
        assert_eq!(taker_state.reserved_margin, UsdValue::ZERO);
        assert_eq!(taker_state.open_order_count, 0);
    }

    // ======== Limit buy IOC: no fills at all → error ========================

    #[test]
    fn limit_buy_ioc_no_fill_errors() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        // Ask at 51,000 — taker limit at 50,000 won't cross.
        place_ask(&mut ctx.storage, MAKER_A, 51_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::ImmediateOrCancel,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("no liquidity at acceptable price")
        );
    }

    // ================== Reduce-only: only closes existing ====================

    #[test]
    fn reduce_only_closes_only() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_bid(&mut ctx.storage, MAKER_A, 48_000, 20, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let mut pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        pair_state.long_oi = Quantity::new_int(5);

        let mut taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };
        taker_state.positions.insert(
            pair_id(),
            Position {
                size: Quantity::new_int(5),
                entry_price: UsdPrice::new_int(50_000),
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            true,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Position fully closed.
        assert!(!taker_state.positions.contains_key(&pair_id()));
        assert!(order_to_store.is_none());
    }

    // ============= Reduce-only with no position errors =======================

    #[test]
    fn reduce_only_no_position_errors() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(10),
            },
            true,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("fillable size is zero")
        );
    }

    // ============= Market sell against resting bids ==========================

    #[test]
    fn market_sell_against_bids() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_bid(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state,
            taker_state,
            order_mutations,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Taker gets a short position.
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(-10));
        assert_eq!(pos.entry_price, UsdPrice::new_int(50_000));

        assert!(order_to_store.is_none());
        assert_eq!(pair_state.short_oi, Quantity::new_int(10));

        // Bid removed from book (1 removal mutation).
        assert_eq!(order_mutations.len(), 1);
        assert!(order_mutations[0].2.is_none());
    }

    // =========== Two-sided settlement: both positions updated =================

    #[test]
    fn two_sided_settlement() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Taker: long 10 @ 50000
        let taker_pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(taker_pos.size, Quantity::new_int(10));

        // Maker: short 10 @ 50000
        let maker_pos = maker_states[&MAKER_A].positions.get(&pair_id()).unwrap();
        assert_eq!(maker_pos.size, Quantity::new_int(-10));
        assert_eq!(maker_pos.entry_price, UsdPrice::new_int(50_000));
    }

    // ============= Fee accounting: net PnLs include fees =====================

    #[test]
    fn pnls_include_fees() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Taker: no realized PnL (opening), fee = |10| * 50000 * 0.001 = $500.
        // Margin decreases by $500.
        assert_eq!(
            taker_state.margin,
            LARGE_COLLATERAL
                .checked_sub(UsdValue::new_int(500))
                .unwrap()
        );

        // Fee goes to vault.
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(500));
    }

    // ======== Tick size enforcement for limit orders =========================

    #[test]
    fn tick_size_valid_multiple_accepted() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let mut pair_param = test_pair_param();
        pair_param.tick_size = UsdPrice::new_int(100);
        PAIR_PARAMS
            .save(&mut ctx.storage, &pair_id(), &pair_param)
            .unwrap();

        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        // 50,100 is a valid multiple of tick size 100 — should succeed.
        let result = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_100),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn tick_size_enforcement() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let mut pair_param = test_pair_param();
        pair_param.tick_size = UsdPrice::new_int(100);
        PAIR_PARAMS
            .save(&mut ctx.storage, &pair_id(), &pair_param)
            .unwrap();

        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_050),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("not a multiple of tick size")
        );
    }

    // ======= Multi-level fill: walks through price levels ====================

    #[test]
    fn market_buy_walks_price_levels() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 5, 100);
        place_ask(&mut ctx.storage, MAKER_B, 50_100, 5, 101);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_mutations,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_100),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(10));

        // Weighted avg entry: (5*50000 + 5*50100) / 10 = 50050
        assert_eq!(pos.entry_price, UsdPrice::new_int(50_050));

        assert!(order_to_store.is_none());

        // Both asks fully filled (2 removal mutations).
        assert_eq!(order_mutations.len(), 2);
        assert!(order_mutations[0].2.is_none());
        assert!(order_mutations[1].2.is_none());
    }

    // ======= Maker reserved margin release: full fill ========================

    #[test]
    fn maker_reserved_margin_released_on_full_fill() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let maker_state_before = USER_STATES.load(&ctx.storage, MAKER_A).unwrap();
        assert!(maker_state_before.reserved_margin.is_non_zero());
        assert_eq!(maker_state_before.open_order_count, 1);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        assert_eq!(maker_states[&MAKER_A].reserved_margin, UsdValue::ZERO);
        assert_eq!(maker_states[&MAKER_A].open_order_count, 0);
    }

    // ======= Maker reserved margin release: partial fill =====================

    #[test]
    fn maker_reserved_margin_released_on_partial_fill() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(4),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        assert_eq!(maker_states[&MAKER_A].open_order_count, 1);

        // initial_margin = 10 * 50000 / 20 = 25000 USD
        // 40% released, 60% remaining = 15000
        assert_eq!(
            maker_states[&MAKER_A].reserved_margin,
            UsdValue::new_int(15_000)
        );
    }

    // ======= Maker reserved margin release: full fill, truncation-prone =====

    /// Regression test for a rounding leak in the proportional
    /// margin-release formula on a full fill.
    ///
    /// The maker's order is planted directly with values chosen so that
    /// `R * S` is not a multiple of `PRECISION` (1e6):
    ///
    /// - `R` (reserved_margin) = 21.406601 USD → inner = 21_406_601
    /// - `S` (|size|)          = 0.182946 qty  → inner =    182_946
    /// - `R.inner * S.inner mod 1_000_000 = 26_546 ≠ 0`
    ///
    /// On a single full fill, the proportional formula
    /// `floor(floor(R·S / 1e6) · 1e6 / S)` yields 21_406_600 — one ULP
    /// short of `R`. The order is removed with a residual ULP still in
    /// `maker_order.reserved_margin`; that residual is dropped from the
    /// order but orphaned in `maker_state.reserved_margin`, breaking the
    /// invariant `user_state.reserved_margin == sum(open orders'
    /// reserved_margin)`.
    ///
    /// After the fix this test must pass: on a full fill, everything
    /// left in the order's reserved_margin is released to the user, so
    /// the user's reserved_margin goes to zero once the only order is
    /// removed.
    #[test]
    fn maker_reserved_margin_no_orphan_on_full_fill_with_truncation() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        // Plant a resting ask directly (can't reuse `place_ask` because it
        // hardcodes integer price/size and computes reserved_margin as
        // `size * price / 20`; we need fractional inner values).
        let price = UsdPrice::new_int(50_000);
        let size_inner: i128 = 182_946; // 0.182946
        let reserved_inner: i128 = 21_406_601; // 21.406601
        let order_id = Uint64::new(100);
        let key = (pair_id(), price, order_id);
        let ask = LimitOrder {
            user: MAKER_A,
            size: Quantity::new_raw(-size_inner),
            reduce_only: false,
            reserved_margin: UsdValue::new_raw(reserved_inner),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        ASKS.save(&mut ctx.storage, key, &ask).unwrap();

        let maker_state_before = UserState {
            margin: LARGE_COLLATERAL,
            open_order_count: 1,
            reserved_margin: ask.reserved_margin,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER_A, &maker_state_before)
            .unwrap();

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome { maker_states, .. } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            price,
            Quantity::new_raw(size_inner),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Order is fully consumed, so the user's reserved_margin must be
        // zero. Pre-fix this is `UsdValue::new_raw(1)` — one ULP orphan.
        assert_eq!(maker_states[&MAKER_A].reserved_margin, UsdValue::ZERO);
        assert_eq!(maker_states[&MAKER_A].open_order_count, 0);
    }

    /// Regression test: a maker ask is consumed by two back-to-back partial
    /// fills that together equal its full size. The final fill lands on the
    /// `new_maker_size.is_zero()` branch and must release every remaining
    /// ULP of `reserved_margin`.
    ///
    /// Pre-fix, each of the two fills truncates via the proportional
    /// formula: the first leaves the order and user state consistent at the
    /// mid-point (both drifted by the same amount), but the second fill
    /// applies the proportional formula again on the residual R, truncates
    /// once more, and drops a ≥1-ULP orphan into `maker_state.reserved_margin`
    /// when the order is removed. Post-fix the final fill releases the full
    /// remainder unconditionally.
    #[test]
    fn maker_reserved_margin_no_orphan_on_full_fill_via_two_partial_fills() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        // Same truncation-prone plant values as the single-shot test above,
        // split into two equal-sized taker fills (2 × 91_473 = 182_946).
        let price = UsdPrice::new_int(50_000);
        let size_inner: i128 = 182_946;
        let half_size_inner: i128 = 91_473;
        let reserved_inner: i128 = 21_406_601;
        let order_id = Uint64::new(100);
        let key = (pair_id(), price, order_id);
        let ask = LimitOrder {
            user: MAKER_A,
            size: Quantity::new_raw(-size_inner),
            reduce_only: false,
            reserved_margin: UsdValue::new_raw(reserved_inner),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        ASKS.save(&mut ctx.storage, key, &ask).unwrap();
        USER_STATES
            .save(
                &mut ctx.storage,
                MAKER_A,
                &UserState {
                    margin: LARGE_COLLATERAL,
                    open_order_count: 1,
                    reserved_margin: ask.reserved_margin,
                    ..Default::default()
                },
            )
            .unwrap();

        let param = test_param();
        let pair_param = test_pair_param();

        // ---------- Phase 1: first half-fill (partial) ----------
        {
            let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
            let taker_state = UserState {
                margin: LARGE_COLLATERAL,
                ..Default::default()
            };

            let SubmitOrderOutcome {
                pair_state,
                taker_state,
                maker_states,
                order_mutations,
                ..
            } = compute_submit_order_outcome(
                &ctx.storage,
                TAKER,
                CONTRACT,
                Timestamp::ZERO,
                &param,
                &State::default(),
                &pair_id(),
                &pair_param,
                &pair_state,
                &taker_state,
                price,
                Quantity::new_raw(half_size_inner),
                OrderKind::Market {
                    max_slippage: Dimensionless::new_permille(100),
                },
                false,
                None,
                None,
                &mut EventBuilder::new(),
            )
            .unwrap();

            // Partial fill: the order must still exist with the residual size.
            assert_eq!(order_mutations.len(), 1);
            assert!(
                order_mutations[0].2.is_some(),
                "order should remain on the book after partial fill"
            );
            assert_eq!(maker_states[&MAKER_A].open_order_count, 1);

            // Persist phase-1 side effects so phase-2 sees the post-partial state.
            PAIR_STATES
                .save(&mut ctx.storage, &pair_id(), &pair_state)
                .unwrap();
            USER_STATES
                .save(&mut ctx.storage, TAKER, &taker_state)
                .unwrap();
            for (addr, ms) in &maker_states {
                USER_STATES.save(&mut ctx.storage, *addr, ms).unwrap();
            }
            for (stored_price, mutated_order_id, mutation, _) in order_mutations {
                let key = (pair_id(), stored_price, mutated_order_id);
                match mutation {
                    Some(order) => ASKS.save(&mut ctx.storage, key, &order).unwrap(),
                    None => ASKS.remove(&mut ctx.storage, key).unwrap(),
                }
            }
        }

        // ---------- Phase 2: consume the remainder (triggers full-fill branch) ----------
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = USER_STATES.load(&ctx.storage, TAKER).unwrap();

        let SubmitOrderOutcome {
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            price,
            Quantity::new_raw(half_size_inner),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Second fill fully consumes the order.
        assert_eq!(order_mutations.len(), 1);
        assert!(
            order_mutations[0].2.is_none(),
            "order should be removed after full consumption"
        );
        // The invariant: no ULP is orphaned in the maker's reserved_margin
        // even though the two fills both touched the truncating formula.
        assert_eq!(maker_states[&MAKER_A].reserved_margin, UsdValue::ZERO);
        assert_eq!(maker_states[&MAKER_A].open_order_count, 0);
    }

    /// Defensive test for the cancel-after-partial-fill pathway. Partial
    /// fills decrement `order.reserved_margin` and `user_state.reserved_margin`
    /// by the same truncated amount, so the mid-life invariant holds by
    /// construction. On cancel, `cancel_order.rs` subtracts the full residual
    /// `order.reserved_margin` from the user's reserved margin; this test
    /// locks in that pairing with fractional-ULP values so a future refactor
    /// cannot silently desynchronize either half.
    ///
    /// Passes both pre- and post-matcher-fix — the matcher bug only manifests
    /// on a *full fill*, which removes the order and bypasses cancel.
    #[test]
    fn reserved_margin_zero_after_cancel_following_partial_fill() {
        use crate::trade::cancel_order::_cancel_one_order;

        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let price = UsdPrice::new_int(50_000);
        let size_inner: i128 = 182_946;
        let half_size_inner: i128 = 91_473;
        let reserved_inner: i128 = 21_406_601;
        let order_id = Uint64::new(100);
        let key = (pair_id(), price, order_id);
        let ask = LimitOrder {
            user: MAKER_A,
            size: Quantity::new_raw(-size_inner),
            reduce_only: false,
            reserved_margin: UsdValue::new_raw(reserved_inner),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        ASKS.save(&mut ctx.storage, key, &ask).unwrap();
        USER_STATES
            .save(
                &mut ctx.storage,
                MAKER_A,
                &UserState {
                    margin: LARGE_COLLATERAL,
                    open_order_count: 1,
                    reserved_margin: ask.reserved_margin,
                    ..Default::default()
                },
            )
            .unwrap();

        // Partial fill consuming half the order.
        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state,
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            price,
            Quantity::new_raw(half_size_inner),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Persist the partial-fill outcome so cancel operates on the correct
        // post-fill state.
        PAIR_STATES
            .save(&mut ctx.storage, &pair_id(), &pair_state)
            .unwrap();
        USER_STATES
            .save(&mut ctx.storage, TAKER, &taker_state)
            .unwrap();
        for (addr, ms) in &maker_states {
            USER_STATES.save(&mut ctx.storage, *addr, ms).unwrap();
        }
        for (stored_price, mutated_order_id, mutation, _) in order_mutations {
            let key = (pair_id(), stored_price, mutated_order_id);
            match mutation {
                Some(order) => ASKS.save(&mut ctx.storage, key, &order).unwrap(),
                None => ASKS.remove(&mut ctx.storage, key).unwrap(),
            }
        }

        // Sanity: after a partial fill the maker still holds some reserved
        // margin — otherwise the cancel path isn't being exercised.
        let mid_state = USER_STATES.load(&ctx.storage, MAKER_A).unwrap();
        assert!(
            mid_state.reserved_margin.is_non_zero(),
            "partial fill must leave non-zero residual reserved_margin"
        );
        assert_eq!(mid_state.open_order_count, 1);

        // Cancel the remaining order.
        let mut events = EventBuilder::new();
        _cancel_one_order(&mut ctx.storage, MAKER_A, order_id, &mut events).unwrap();

        let after = USER_STATES.load(&ctx.storage, MAKER_A).unwrap();
        assert_eq!(after.reserved_margin, UsdValue::ZERO);
        assert_eq!(after.open_order_count, 0);
    }

    /// Defensive test for the self-trade prevention (STP) pathway. When a
    /// taker crosses their own maker, the STP branch releases the full
    /// `maker_order.reserved_margin` in one step (no truncating formula),
    /// so the taker's `reserved_margin` must land at exactly zero — even
    /// when the original reservation uses fractional-ULP values. This
    /// tightens the existing `self_trade_prevention_expire_maker` test,
    /// which only asserts `taker_state.reserved_margin < taker_reserved_before`.
    ///
    /// Passes both pre- and post-matcher-fix — the STP branch is untouched
    /// by the fix.
    #[test]
    fn reserved_margin_zero_after_self_trade_prevention_with_fractional_values() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        // TAKER owns a resting ask planted with fractional reserved_margin.
        let takers_price = UsdPrice::new_int(50_000);
        let size_inner: i128 = 182_946;
        let reserved_inner: i128 = 21_406_601;
        let takers_order_id = Uint64::new(100);
        let takers_key = (pair_id(), takers_price, takers_order_id);
        let takers_ask = LimitOrder {
            user: TAKER,
            size: Quantity::new_raw(-size_inner),
            reduce_only: false,
            reserved_margin: UsdValue::new_raw(reserved_inner),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        ASKS.save(&mut ctx.storage, takers_key, &takers_ask)
            .unwrap();

        // A second, higher-priced maker ask so the taker's market buy can
        // find liquidity after STP cancels the taker's own ask (otherwise
        // `compute_submit_order_outcome` rejects the order as "no liquidity
        // at acceptable price" when `unfilled == fillable_size`).
        place_ask(&mut ctx.storage, MAKER_A, 50_100, 10, 101);

        let taker_state_before = UserState {
            margin: LARGE_COLLATERAL,
            open_order_count: 1,
            reserved_margin: takers_ask.reserved_margin,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, TAKER, &taker_state_before)
            .unwrap();

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();

        let SubmitOrderOutcome {
            taker_state,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state_before,
            UsdPrice::new_int(50_100),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // First mutation is the taker's own ask, removed by STP.
        assert_eq!(order_mutations.len(), 2);
        assert!(
            order_mutations[0].2.is_none(),
            "taker's own ask should be STP-cancelled (mutation = None)"
        );

        // The STP branch releases the full fractional reservation in one
        // step — no truncation, no orphan.
        assert_eq!(taker_state.reserved_margin, UsdValue::ZERO);
        assert_eq!(taker_state.open_order_count, 0);
    }

    // ======= Market buy: price beyond slippage ===============================

    #[test]
    fn market_buy_slippage_exceeded() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 60_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(10), // 1%
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("no liquidity at acceptable price")
        );
    }

    // =================== settle_pnls unit tests ==============================

    /// Perform one synthetic fill via per-fill `settle_pnls`, routing the
    /// vault's UserState from the caller's `maker_states` map. Returns the
    /// per-party fee breakdowns for the fill.
    fn settle_one_fill(
        state: &mut State,
        param: &Param,
        taker: Addr,
        taker_state: &mut UserState,
        taker_pnl: UsdValue,
        taker_fee: UsdValue,
        maker: Addr,
        maker_states: &mut BTreeMap<Addr, UserState>,
        maker_pnl: UsdValue,
        maker_fee: UsdValue,
    ) -> anyhow::Result<FillFeeBreakdowns> {
        // Temporarily pull the maker's state out so `vault_state` can be
        // borrowed from the map disjointly. Reinsert afterwards.
        let mut maker_state = maker_states.remove(&maker).unwrap_or_default();

        let result = if maker == CONTRACT {
            settle_pnls(
                CONTRACT,
                param,
                state,
                taker,
                taker_state,
                taker_pnl,
                taker_fee,
                maker,
                &mut maker_state,
                maker_pnl,
                maker_fee,
                None,
            )
        } else {
            let vault_state = maker_states
                .get_mut(&CONTRACT)
                .expect("vault must be present in maker_states for a non-vault maker");
            settle_pnls(
                CONTRACT,
                param,
                state,
                taker,
                taker_state,
                taker_pnl,
                taker_fee,
                maker,
                &mut maker_state,
                maker_pnl,
                maker_fee,
                Some(vault_state),
            )
        };

        maker_states.insert(maker, maker_state);
        result
    }

    #[test]
    fn settle_pnls_mixed() {
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
            (Addr::mock(3), UserState::default()),
        ]);
        let mut state = State::default();

        // Fill 1: taker vs maker 2. Taker realises +$100 PnL, maker 2
        // realises −$200 PnL on this fill.
        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::new_int(100),
            UsdValue::ZERO,
            Addr::mock(2),
            &mut maker_states,
            UsdValue::new_int(-200),
            UsdValue::ZERO,
        )
        .unwrap();

        // Fill 2: taker vs maker 3. No PnL, no fee — mirrors the old
        // per-user `pnls` entry of ZERO for maker 3.
        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::ZERO,
            Addr::mock(3),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        // Positive PnL: taker margin += $100.
        assert_eq!(taker_state.margin, UsdValue::new_int(100));

        // Negative PnL: user 2 margin -= $200.
        assert_eq!(maker_states[&Addr::mock(2)].margin, UsdValue::new_int(-200));

        // Zero PnL: user 3 margin unchanged.
        assert_eq!(maker_states[&Addr::mock(3)].margin, UsdValue::ZERO);

        // Non-vault PnL does not change vault margin.
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::ZERO);
    }

    #[test]
    fn settle_pnls_all_payouts() {
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::new_int(100),
            UsdValue::ZERO,
            Addr::mock(2),
            &mut maker_states,
            UsdValue::new_int(50),
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(taker_state.margin, UsdValue::new_int(100));
        assert_eq!(maker_states[&Addr::mock(2)].margin, UsdValue::new_int(50));

        // Non-vault PnL does not change vault margin.
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::ZERO);
    }

    #[test]
    fn settle_pnls_all_collections() {
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::new_int(-100),
            UsdValue::ZERO,
            Addr::mock(2),
            &mut maker_states,
            UsdValue::new_int(-200),
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(taker_state.margin, UsdValue::new_int(-100));
        assert_eq!(maker_states[&Addr::mock(2)].margin, UsdValue::new_int(-200));

        // Non-vault PnL does not change vault margin.
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::ZERO);
    }

    #[test]
    fn settle_pnls_empty() {
        let taker = TAKER;
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([(
            CONTRACT,
            UserState {
                margin: UsdValue::new_int(500),
                ..Default::default()
            },
        )]);
        let mut state = State::default();

        // No fills — nothing to settle. Vault margin should be unchanged.
        let _ = (
            &mut state,
            &mut taker_state,
            &mut maker_states,
            taker,
            Param::default(),
        );

        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(500));
    }

    #[test]
    fn settle_pnls_fees_increase_vault_margin() {
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        // Single fill: taker pays $50, maker 2 pays $100. `protocol_fee_rate`
        // defaults to 0, so the full $150 goes to the vault.
        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::new_int(50),
            Addr::mock(2),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::new_int(100),
        )
        .unwrap();

        // Taker's margin decreases by fee amount.
        assert_eq!(taker_state.margin, UsdValue::new_int(-50));

        // Maker's margin decreases by fee amount.
        assert_eq!(maker_states[&Addr::mock(2)].margin, UsdValue::new_int(-100));

        // Fees go to vault margin: $50 + $100 = $150.
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(150));
    }

    #[test]
    fn settle_pnls_vault_pnl_adjusts_margin() {
        let taker = TAKER;
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([(
            CONTRACT,
            UserState {
                margin: UsdValue::new_int(1_000),
                ..Default::default()
            },
        )]);
        let mut state = State::default();

        // Vault is the maker on this fill and realises +$500 PnL.
        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::ZERO,
            CONTRACT,
            &mut maker_states,
            UsdValue::new_int(500),
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(1_500));
    }

    #[test]
    fn settle_pnls_vault_loss_creates_bad_debt() {
        let taker = TAKER;
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([(
            CONTRACT,
            UserState {
                margin: UsdValue::new_int(100),
                ..Default::default()
            },
        )]);
        let mut state = State::default();

        // Vault is the maker and realises −$500 PnL.
        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::ZERO,
            CONTRACT,
            &mut maker_states,
            UsdValue::new_int(-500),
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(-400));
    }

    #[test]
    fn settle_pnls_vault_profit_recovers_negative_margin() {
        let taker = TAKER;
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([(
            CONTRACT,
            UserState {
                margin: UsdValue::new_int(-300),
                ..Default::default()
            },
        )]);
        let mut state = State::default();

        // Vault maker with +$500 PnL on this fill.
        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::ZERO,
            CONTRACT,
            &mut maker_states,
            UsdValue::new_int(500),
            UsdValue::ZERO,
        )
        .unwrap();

        // Vault margin recovers: -300 + 500 = 200.
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(200));
    }

    #[test]
    fn settle_pnls_vault_fees_skipped() {
        let taker = TAKER;
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([(
            CONTRACT,
            UserState {
                margin: UsdValue::new_int(1_000),
                ..Default::default()
            },
        )]);
        let mut state = State::default();

        // Vault is the maker with no PnL and no fee — the upstream
        // `settle_fill` invariant guarantees vault makers always supply
        // `maker_fee = 0`, so `settle_pnls` has no fee to "skip" and the
        // vault's margin stays put.
        settle_one_fill(
            &mut state,
            &Param::default(),
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::ZERO,
            CONTRACT,
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(1_000));
    }

    #[test]
    fn settle_pnls_protocol_fee_split() {
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let param = Param {
            protocol_fee_rate: Dimensionless::new_percent(20),
            ..Default::default()
        };
        let mut maker_states = BTreeMap::from([(CONTRACT, UserState::default())]);
        let mut state = State::default();

        // Taker pays $100, vault is the maker with no fee. Net = $100 →
        // treasury 20% ($20), vault 80% ($80).
        settle_one_fill(
            &mut state,
            &param,
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::new_int(100),
            CONTRACT,
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert_eq!(taker_state.margin, UsdValue::new_int(-100));
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(80));
        assert_eq!(state.treasury, UsdValue::new_int(20));
    }

    // =========== FeeBreakdown match-arm tests ===============
    //
    // One dedicated test per arm of the
    // `(taker_fee.is_positive(), maker_fee.is_positive())` match in
    // `settle_pnls`. Each isolates the breakdown invariant for that arm.

    #[test]
    fn settle_pnls_breakdown_both_positive() {
        // (true, true): taker takes weight × pool, maker takes the residual,
        // sum equals the credited amount exactly — even when the weight
        // isn't representable in Dec128_6.
        //
        // taker_fee = 7 µUSD, maker_fee = 3 µUSD, protocol_fee_rate = 20%
        //   → net_fee = 10, protocol_fee = 2, vault_fee = 8
        //   → weight_taker = 0.7, vault_fee × 0.7 = 5.6 → truncates to 5
        //   → weight_maker = 0.3, vault_fee × 0.3 = 2.4 → truncates to 2
        //   → independent multiplies leak 1 µUSD; subtraction does not.
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let param = Param {
            protocol_fee_rate: Dimensionless::new_percent(20),
            ..Default::default()
        };
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        let breakdowns = settle_one_fill(
            &mut state,
            &param,
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::new_raw(7),
            Addr::mock(2),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::new_raw(3),
        )
        .unwrap();

        let taker_bd = breakdowns.taker.unwrap();
        let maker_bd = breakdowns.maker.unwrap();

        // Chain credited treasury with full protocol_fee, vault with full
        // vault_fee.
        assert_eq!(state.treasury, UsdValue::new_raw(2));
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_raw(8));

        // Per-payer breakdowns sum exactly to the credited amounts.
        assert_eq!(
            taker_bd
                .protocol_fee
                .checked_add(maker_bd.protocol_fee)
                .unwrap(),
            state.treasury,
        );
        assert_eq!(
            taker_bd.vault_fee.checked_add(maker_bd.vault_fee).unwrap(),
            maker_states[&CONTRACT].margin,
        );
    }

    #[test]
    fn settle_pnls_breakdown_only_taker_positive() {
        // (true, false): taker takes the full pool literally (no
        // weight-multiply); maker (rebater) breakdown is exactly zero so
        // no rounding can drift onto it.
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let param = Param {
            protocol_fee_rate: Dimensionless::new_percent(20),
            ..Default::default()
        };
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        // Taker fee = +$30, maker fee = −$10 (rebate).
        let breakdowns = settle_one_fill(
            &mut state,
            &param,
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::new_int(30),
            Addr::mock(2),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::new_int(-10),
        )
        .unwrap();

        let taker_bd = breakdowns.taker.unwrap();
        let maker_bd = breakdowns.maker.unwrap();

        // Taker absorbs the full pool; rebater is exactly zero. Rebater is
        // emitted as `Some(zero)` so apply_fee_commissions still tracks
        // their referral volume.
        assert_eq!(taker_bd.protocol_fee, state.treasury);
        assert_eq!(taker_bd.vault_fee, maker_states[&CONTRACT].margin);
        assert_eq!(maker_bd.protocol_fee, UsdValue::ZERO);
        assert_eq!(maker_bd.vault_fee, UsdValue::ZERO);
    }

    #[test]
    fn settle_pnls_breakdown_only_maker_positive() {
        // (false, true): maker takes the full pool; taker (rebater) is
        // exactly zero. Symmetric to `only_taker_positive`.
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let param = Param {
            protocol_fee_rate: Dimensionless::new_percent(20),
            ..Default::default()
        };
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        // Taker fee = −$10 (rebate), maker fee = +$30.
        let breakdowns = settle_one_fill(
            &mut state,
            &param,
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::new_int(-10),
            Addr::mock(2),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::new_int(30),
        )
        .unwrap();

        let taker_bd = breakdowns.taker.unwrap();
        let maker_bd = breakdowns.maker.unwrap();

        // Maker absorbs the full pool; rebater is exactly zero.
        assert_eq!(maker_bd.protocol_fee, state.treasury);
        assert_eq!(maker_bd.vault_fee, maker_states[&CONTRACT].margin);
        assert_eq!(taker_bd.protocol_fee, UsdValue::ZERO);
        assert_eq!(taker_bd.vault_fee, UsdValue::ZERO);
    }

    #[test]
    fn settle_pnls_breakdown_neither_positive() {
        // (false, false): both fees zero → both breakdowns zero; both emit
        // `None` (no per-payer event for parties that owed no fee). Treasury
        // and vault see no movement.
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let param = Param {
            protocol_fee_rate: Dimensionless::new_percent(20),
            ..Default::default()
        };
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        let breakdowns = settle_one_fill(
            &mut state,
            &param,
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::ZERO,
            Addr::mock(2),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::ZERO,
        )
        .unwrap();

        assert!(breakdowns.taker.is_none());
        assert!(breakdowns.maker.is_none());
        assert_eq!(state.treasury, UsdValue::ZERO);
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::ZERO);
    }

    // =========== Negative maker fee (rebate) settle_pnls tests ===============

    #[test]
    fn settle_pnls_negative_maker_fee_rebate() {
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        // Taker pays +$50 fee, maker receives -$10 fee (rebate).
        settle_one_fill(
            &mut state,
            &Param::default(), // protocol_fee_rate = 0
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::new_int(50),
            Addr::mock(2),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::new_int(-10),
        )
        .unwrap();

        // Taker margin decreases by fee.
        assert_eq!(taker_state.margin, UsdValue::new_int(-50));

        // Maker margin increases (rebate): 0 - (-10) = +10.
        assert_eq!(maker_states[&Addr::mock(2)].margin, UsdValue::new_int(10));

        // Vault receives net: +50 (from taker) + (-10) (rebate to maker) = +40.
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(40));

        // No protocol fee split → treasury unchanged.
        assert_eq!(state.treasury, UsdValue::ZERO);
    }

    #[test]
    fn settle_pnls_negative_maker_fee_with_protocol_split() {
        let taker = Addr::mock(1);
        let mut taker_state = UserState::default();
        let param = Param {
            protocol_fee_rate: Dimensionless::new_percent(20),
            ..Default::default()
        };
        let mut maker_states = BTreeMap::from([
            (CONTRACT, UserState::default()),
            (Addr::mock(2), UserState::default()),
        ]);
        let mut state = State::default();

        // Taker fee = +$30, maker fee = -$10 (rebate). Net = $20.
        let breakdowns = settle_one_fill(
            &mut state,
            &param,
            taker,
            &mut taker_state,
            UsdValue::ZERO,
            UsdValue::new_int(30),
            Addr::mock(2),
            &mut maker_states,
            UsdValue::ZERO,
            UsdValue::new_int(-10),
        )
        .unwrap();

        // Taker pays full fee.
        assert_eq!(taker_state.margin, UsdValue::new_int(-30));

        // Maker gets rebate: 0 - (-10) = +10.
        assert_eq!(maker_states[&Addr::mock(2)].margin, UsdValue::new_int(10));

        // Vault receives:
        //   from taker: $30 * 80% = $24
        //   from maker: -$10 * 80% = -$8
        //   total = $16
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(16));

        // Treasury receives:
        //   from taker: $30 * 20% = $6
        //   from maker: -$10 * 20% = -$2
        //   total = $4
        assert_eq!(state.treasury, UsdValue::new_int(4));

        // Rebater's breakdown is exactly zero — the (true, false) match arm
        // assigns the full pool to the taker and a literal zero to the
        // maker, so no `checked_mul` runs that could drift a rounding
        // residual onto the rebater.
        let taker_bd = breakdowns.taker.unwrap();
        let maker_bd = breakdowns.maker.unwrap();
        assert_eq!(maker_bd.protocol_fee, UsdValue::ZERO);
        assert_eq!(maker_bd.vault_fee, UsdValue::ZERO);
        assert_eq!(taker_bd.protocol_fee, state.treasury);
        assert_eq!(taker_bd.vault_fee, maker_states[&CONTRACT].margin);
    }

    // ====== Negative maker fee: full compute_submit_order_outcome integration ===============

    #[test]
    fn negative_maker_fee_rebate_on_market_buy() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        // Custom param: taker = 3 bps, maker = -1 bps, protocol = 20%.
        let param = Param {
            max_open_orders: 10,
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_raw(300), // 3 bps
                ..Default::default()
            },
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_raw(-100), // -1 bps
                ..Default::default()
            },
            protocol_fee_rate: Dimensionless::new_percent(20),
            ..Default::default()
        };

        // Manually set up storage (can't use setup_storage which saves test_param).
        PARAM.save(&mut ctx.storage, &param).unwrap();
        PAIR_PARAMS
            .save(&mut ctx.storage, &pair_id(), &test_pair_param())
            .unwrap();
        PAIR_STATES
            .save(
                &mut ctx.storage,
                &pair_id(),
                &PairState {
                    oracle_price: UsdPrice::new_int(50_000),
                    ..Default::default()
                },
            )
            .unwrap();
        NEXT_ORDER_ID
            .save(&mut ctx.storage, &Uint64::new(1))
            .unwrap();
        NEXT_FILL_ID.save(&mut ctx.storage, &FillId::ONE).unwrap();

        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };
        let state = State::default();

        let SubmitOrderOutcome {
            state,
            pair_state: _,
            taker_state,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &state,
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Notional = 10 * $50,000 = $500,000.
        // Taker fee = $500,000 * 3 bps = $150.
        // Maker fee = $500,000 * (-1 bps) = -$50 (rebate).
        //
        // Taker margin: LARGE_COLLATERAL - $150.
        assert_eq!(
            taker_state.margin,
            LARGE_COLLATERAL
                .checked_sub(UsdValue::new_int(150))
                .unwrap()
        );

        // Maker margin: 0 - (-$50) = +$50 (receives rebate).
        assert_eq!(maker_states[&MAKER_A].margin, UsdValue::new_int(50));

        // Vault receives:
        //   taker vault_fee = $150 * 80% = $120
        //   maker vault_fee = -$50 * 80% = -$40
        //   total = $80
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(80));

        // Treasury:
        //   taker protocol_fee = $150 * 20% = $30
        //   maker protocol_fee = -$50 * 20% = -$10
        //   total = $20
        assert_eq!(state.treasury, UsdValue::new_int(20));

        // Positions are correct.
        let taker_pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(taker_pos.size, Quantity::new_int(10));
        assert_eq!(taker_pos.entry_price, UsdPrice::new_int(50_000));
    }

    // =================== Fee rate overrides ==================================

    /// Taker fills two consecutive orders. Between the fills, the admin sets
    /// a lower taker-fee override on the taker. Fill #1 must be charged the
    /// schedule rate (5 bps); fill #2 must be charged the override rate (1 bps).
    #[test]
    fn fee_override_taker_rate_applied() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        // Taker fee = 5 bps on the schedule; maker fee & protocol fee = 0
        // so that fee flows land entirely on the taker/vault legs and the
        // numbers are easy to reason about.
        let param = Param {
            max_open_orders: 10,
            taker_fee_rates: RateSchedule {
                base: Dimensionless::new_raw(500), // 5 bps
                ..Default::default()
            },
            ..Default::default()
        };

        PARAM.save(&mut ctx.storage, &param).unwrap();
        PAIR_PARAMS
            .save(&mut ctx.storage, &pair_id(), &test_pair_param())
            .unwrap();
        PAIR_STATES
            .save(
                &mut ctx.storage,
                &pair_id(),
                &PairState {
                    oracle_price: UsdPrice::new_int(50_000),
                    ..Default::default()
                },
            )
            .unwrap();
        NEXT_ORDER_ID
            .save(&mut ctx.storage, &Uint64::new(1))
            .unwrap();
        NEXT_FILL_ID.save(&mut ctx.storage, &FillId::ONE).unwrap();

        // Two resting asks — one per phase.
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 101);

        let pair_param = test_pair_param();

        // ----------------- Phase 1: no override, schedule rate -----------------

        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state,
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Notional = 10 × $50,000 = $500,000. Taker fee = 5 bps × $500k = $250.
        assert_eq!(
            taker_state.margin,
            LARGE_COLLATERAL
                .checked_sub(UsdValue::new_int(250))
                .unwrap()
        );
        assert_eq!(maker_states[&MAKER_A].margin, UsdValue::ZERO);
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(250));

        // Persist side effects so phase 3 sees the post-phase-1 state.
        PAIR_STATES
            .save(&mut ctx.storage, &pair_id(), &pair_state)
            .unwrap();
        USER_STATES
            .save(&mut ctx.storage, TAKER, &taker_state)
            .unwrap();
        for (addr, ms) in &maker_states {
            USER_STATES.save(&mut ctx.storage, *addr, ms).unwrap();
        }
        for (stored_price, order_id, mutation, _) in order_mutations {
            let key = (pair_id(), stored_price, order_id);
            match mutation {
                Some(order) => ASKS.save(&mut ctx.storage, key, &order).unwrap(),
                None => ASKS.remove(&mut ctx.storage, key).unwrap(),
            }
        }

        // ------------------ Phase 2: apply the taker override ------------------

        // 1 bps — lower than the 5 bps schedule rate, matching the realistic
        // VIP-discount use case.
        FEE_RATE_OVERRIDES
            .save(
                &mut ctx.storage,
                TAKER,
                &(Dimensionless::ZERO, Dimensionless::new_raw(100)),
            )
            .unwrap();

        // ------------------ Phase 3: override-branch fill ----------------------

        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = USER_STATES.load(&ctx.storage, TAKER).unwrap();

        let SubmitOrderOutcome {
            taker_state,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Fill #2 fee = 1 bps × $500k = $50 (override). Without the override,
        // it would have been another $250 and the totals below would be $500.
        //
        // Cumulative: taker paid $250 + $50 = $300; vault received $300.
        assert_eq!(
            taker_state.margin,
            LARGE_COLLATERAL
                .checked_sub(UsdValue::new_int(300))
                .unwrap()
        );
        assert_eq!(maker_states[&MAKER_A].margin, UsdValue::ZERO);
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(300));
    }

    /// MAKER_A's two resting asks are each consumed by a taker market buy.
    /// Between the fills, the admin sets a lower maker-fee override on
    /// MAKER_A. Fill #1 must be charged the schedule rate (5 bps);
    /// fill #2 must be charged the override rate (1 bps).
    #[test]
    fn fee_override_maker_rate_applied() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        // Maker fee = 5 bps on the schedule; taker fee & protocol fee = 0
        // so the maker is the only party paying a fee and assertions isolate
        // the maker-side override.
        let param = Param {
            max_open_orders: 10,
            maker_fee_rates: RateSchedule {
                base: Dimensionless::new_raw(500), // 5 bps
                ..Default::default()
            },
            ..Default::default()
        };

        PARAM.save(&mut ctx.storage, &param).unwrap();
        PAIR_PARAMS
            .save(&mut ctx.storage, &pair_id(), &test_pair_param())
            .unwrap();
        PAIR_STATES
            .save(
                &mut ctx.storage,
                &pair_id(),
                &PairState {
                    oracle_price: UsdPrice::new_int(50_000),
                    ..Default::default()
                },
            )
            .unwrap();
        NEXT_ORDER_ID
            .save(&mut ctx.storage, &Uint64::new(1))
            .unwrap();
        NEXT_FILL_ID.save(&mut ctx.storage, &FillId::ONE).unwrap();

        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 101);

        let pair_param = test_pair_param();

        // ----------------- Phase 1: no override, schedule rate -----------------

        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state,
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Notional = $500,000. Maker fee = 5 bps × $500k = $250.
        // MAKER_A starts at margin 0 (place_ask doesn't seed margin), so the
        // post-fill margin is -$250.
        assert_eq!(taker_state.margin, LARGE_COLLATERAL);
        assert_eq!(maker_states[&MAKER_A].margin, UsdValue::new_int(-250));
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(250));

        PAIR_STATES
            .save(&mut ctx.storage, &pair_id(), &pair_state)
            .unwrap();
        USER_STATES
            .save(&mut ctx.storage, TAKER, &taker_state)
            .unwrap();
        for (addr, ms) in &maker_states {
            USER_STATES.save(&mut ctx.storage, *addr, ms).unwrap();
        }
        for (stored_price, order_id, mutation, _) in order_mutations {
            let key = (pair_id(), stored_price, order_id);
            match mutation {
                Some(order) => ASKS.save(&mut ctx.storage, key, &order).unwrap(),
                None => ASKS.remove(&mut ctx.storage, key).unwrap(),
            }
        }

        // ------------------ Phase 2: apply the maker override ------------------

        FEE_RATE_OVERRIDES
            .save(
                &mut ctx.storage,
                MAKER_A,
                &(Dimensionless::new_raw(100), Dimensionless::ZERO), // 1 bps
            )
            .unwrap();

        // ------------------ Phase 3: override-branch fill ----------------------

        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = USER_STATES.load(&ctx.storage, TAKER).unwrap();

        let SubmitOrderOutcome {
            taker_state,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Fill #2 maker fee = 1 bps × $500k = $50 (override). Without the
        // override it would have been another $250, and MAKER_A's final
        // margin would be -$500.
        //
        // Cumulative: MAKER_A paid $250 + $50 = $300; vault received $300.
        assert_eq!(taker_state.margin, LARGE_COLLATERAL);
        assert_eq!(maker_states[&MAKER_A].margin, UsdValue::new_int(-300));
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(300));
    }

    // =================== Post-only order tests ===============================

    #[test]
    fn post_only_buy_rests_below_best_ask() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_mutations,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(49_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // No fills — order rests.
        assert!(!taker_state.positions.contains_key(&pair_id()));
        assert!(order_to_store.is_some());
        let (_, _, order) = order_to_store.unwrap();
        assert_eq!(order.size, Quantity::new_int(10));

        // No mutations.
        assert!(order_mutations.is_empty());
    }

    #[test]
    fn post_only_buy_at_best_ask_rejected() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("post-only buy"));
    }

    #[test]
    fn post_only_buy_above_best_ask_rejected() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(51_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("post-only buy"));
    }

    #[test]
    fn post_only_sell_rests_above_best_bid() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_bid(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            order_mutations,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(51_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // No fills — order rests.
        assert!(!taker_state.positions.contains_key(&pair_id()));
        assert!(order_to_store.is_some());
        let (_, _, order) = order_to_store.unwrap();
        assert_eq!(order.size, Quantity::new_int(-10));

        assert!(order_mutations.is_empty());
    }

    #[test]
    fn post_only_sell_at_best_bid_rejected() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_bid(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("post-only sell"));
    }

    #[test]
    fn post_only_buy_empty_book_succeeds() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        // No asks placed — empty book.

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(49_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        assert!(order_to_store.is_some());
        let (_, _, order) = order_to_store.unwrap();
        assert_eq!(order.size, Quantity::new_int(10));
    }

    #[test]
    fn post_only_reduce_only_rests() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_bid(&mut ctx.storage, MAKER_A, 48_000, 20, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let mut pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        pair_state.long_oi = Quantity::new_int(5);

        let mut taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };
        taker_state.positions.insert(
            pair_id(),
            Position {
                size: Quantity::new_int(5),
                entry_price: UsdPrice::new_int(50_000),
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-5),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(51_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            true,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Order rests, reduce_only flag preserved.
        assert!(order_to_store.is_some());
        let (_, _, order) = order_to_store.unwrap();
        assert_eq!(order.size, Quantity::new_int(-5));
        assert!(order.reduce_only);
    }

    // ======= Post-only insufficient margin rejected ==========================

    /// Post-only buy with insufficient collateral is rejected by the 0%-fill
    /// margin check inside `store_limit_order`.
    ///
    /// pair: BTC, oracle = $50,000, IMR = 5%
    /// Buy 10 BTC post-only @ $49,000
    ///   margin_to_reserve = |10| * 49,000 * 0.05 = $24,500
    ///   collateral = $1,000 → available = $1,000 < $24,500 → FAILS
    #[test]
    fn post_only_insufficient_margin_rejected() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: UsdValue::new_int(1_000), // insufficient collateral
            ..Default::default()
        };

        let err = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(49_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        );

        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("insufficient margin for limit order"),
            "expected limit-order margin error, got: {msg}"
        );
    }

    // ======= Self-trade prevention (EXPIRE_MAKER) ========================

    #[test]
    fn self_trade_prevention_expire_maker() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        // TAKER has a resting ask at 50,000 (order_id 100).
        place_ask(&mut ctx.storage, TAKER, 50_000, 10, 100);
        // MAKER_A has a resting ask behind it at 50,100 (order_id 101).
        place_ask(&mut ctx.storage, MAKER_A, 50_100, 10, 101);

        // Snapshot taker state after placing the resting order.
        let taker_state_before = USER_STATES.load(&ctx.storage, TAKER).unwrap();
        assert_eq!(taker_state_before.open_order_count, 1);
        let taker_reserved_before = taker_state_before.reserved_margin;

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();

        // Start with the taker state from storage (has the resting order's
        // reserved margin and open_order_count).
        let mut taker_state = taker_state_before.clone();
        taker_state.margin = LARGE_COLLATERAL;

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_100),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100), // 10%
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // 1) Taker's own ask was cancelled (mutation = None).
        assert_eq!(order_mutations.len(), 2);
        assert!(
            order_mutations[0].2.is_none(),
            "taker's resting order should be removed"
        );

        // 2) Taker's open_order_count was decremented (the resting order
        //    was cancelled by STP).
        assert_eq!(taker_state.open_order_count, 0);

        // 3) Taker's reserved margin from the cancelled order was released.
        assert!(taker_reserved_before.is_non_zero());
        // The reserved_margin for the cancelled order is fully released.
        // Only trading-fee deductions (from the fill against MAKER_A) remain.
        assert!(taker_state.reserved_margin < taker_reserved_before);

        // 4) Taker DID fill against the second maker's ask at 50,100.
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(10));
        assert_eq!(pos.entry_price, UsdPrice::new_int(50_100));

        // 5) MAKER_A's ask was fully filled (second mutation = None).
        assert!(
            order_mutations[1].2.is_none(),
            "MAKER_A's order should be fully filled"
        );
        assert_eq!(maker_states[&MAKER_A].open_order_count, 0);
    }

    // ======= Match-time price-band re-check ==============================

    /// Far-end drift: a stale maker whose price is above the upper band
    /// bound is encountered after in-band makers. The walk fills the
    /// in-band maker first, then cancels the stale maker on encounter
    /// and continues (terminating when it runs out of book).
    #[test]
    fn drift_cancel_far_end_out_of_band_maker() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());
        setup_storage(&mut ctx.storage);

        // MAKER_A at $35,000 (in-band at oracle=$30,000, 50% band → [15k, 45k]).
        place_ask(&mut ctx.storage, MAKER_A, 35_000, 5, 100);
        // MAKER_B at $50,000 (above upper band). Stale from when oracle was higher.
        place_ask(&mut ctx.storage, MAKER_B, 50_000, 10, 101);

        let maker_b_reserved_before = USER_STATES
            .load(&ctx.storage, MAKER_B)
            .unwrap()
            .reserved_margin;

        let param = test_param();
        let pair_param = test_pair_param();
        // Model an oracle that has moved to $30k; stale makers from when it was
        // higher now fall outside the band around it.
        let pair_state = PairState {
            oracle_price: UsdPrice::new_int(30_000),
            ..PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap()
        };
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            // Oracle at $30,000, band = 50% → allowed [$15k, $45k].
            UsdPrice::new_int(30_000),
            Quantity::new_int(15),
            OrderKind::Market {
                // Wide slippage so target_price doesn't terminate the walk
                // before reaching the $50k maker: target = $30k * (1 + 0.99)
                // = ~$59.7k > $50k.
                max_slippage: Dimensionless::new_percent(99),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Two mutations: MAKER_A fully filled (None), MAKER_B cancelled (None).
        assert_eq!(order_mutations.len(), 2);
        assert!(order_mutations[0].2.is_none(), "MAKER_A fully filled");
        assert!(order_mutations[1].2.is_none(), "MAKER_B cancelled");

        // MAKER_B's resting order was cancelled via the drift check.
        assert_eq!(maker_states[&MAKER_B].open_order_count, 0);
        assert!(
            maker_b_reserved_before.is_non_zero()
                && maker_states[&MAKER_B].reserved_margin < maker_b_reserved_before,
            "MAKER_B's reserved margin should be fully released"
        );

        // Taker filled 5 @ $35,000 against MAKER_A only; MAKER_B was
        // cancelled, not filled.
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(5));
        assert_eq!(pos.entry_price, UsdPrice::new_int(35_000));
    }

    /// Near-end drift: a stale maker whose price is below the lower band
    /// bound is encountered *before* in-band makers. The walk cancels the
    /// stale maker (rather than breaking) and proceeds to the in-band
    /// maker behind it. This is the case `continue` handles correctly that
    /// `break` would miss.
    #[test]
    fn drift_cancel_near_end_out_of_band_maker() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());
        setup_storage(&mut ctx.storage);

        // MAKER_A at $5,000 — below oracle's lower bound (stale from when
        // oracle was much lower). Walked *first* in ascending order.
        place_ask(&mut ctx.storage, MAKER_A, 5_000, 5, 100);
        // MAKER_B at $35,000 — in-band at oracle=$30,000, 50% band.
        place_ask(&mut ctx.storage, MAKER_B, 35_000, 10, 101);

        let maker_a_reserved_before = USER_STATES
            .load(&ctx.storage, MAKER_A)
            .unwrap()
            .reserved_margin;

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(30_000),
            Quantity::new_int(15),
            OrderKind::Market {
                max_slippage: Dimensionless::new_percent(99),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Two mutations: MAKER_A cancelled first, MAKER_B fully filled.
        assert_eq!(order_mutations.len(), 2);
        assert!(order_mutations[0].2.is_none(), "MAKER_A cancelled");
        assert!(order_mutations[1].2.is_none(), "MAKER_B fully filled");

        // MAKER_A: cancelled, reserved released.
        assert_eq!(maker_states[&MAKER_A].open_order_count, 0);
        assert!(
            maker_a_reserved_before.is_non_zero()
                && maker_states[&MAKER_A].reserved_margin < maker_a_reserved_before,
            "MAKER_A's reserved margin should be fully released"
        );

        // Taker filled 10 @ $35,000 against MAKER_B (walked past cancelled A).
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(10));
        assert_eq!(pos.entry_price, UsdPrice::new_int(35_000));
    }

    /// Vault exemption: a vault maker whose price is outside the band is
    /// NOT cancelled by the match-time check. The vault's prices are
    /// algorithmically bounded and auto-refresh; cancelling them on match
    /// would cause churn without security gain.
    #[test]
    fn drift_cancel_skips_vault_maker() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());
        setup_storage(&mut ctx.storage);

        // Vault (CONTRACT) is the maker. Give it enough margin to take
        // the other side of the taker's buy.
        USER_STATES
            .save(
                &mut ctx.storage,
                CONTRACT,
                &UserState {
                    margin: LARGE_COLLATERAL,
                    ..Default::default()
                },
            )
            .unwrap();
        // Vault ask at $50,000 — would be out-of-band at oracle=$30k.
        place_ask(&mut ctx.storage, CONTRACT, 50_000, 5, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            taker_state,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(30_000),
            Quantity::new_int(5),
            OrderKind::Market {
                max_slippage: Dimensionless::new_percent(99),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Vault maker was filled (not cancelled) despite being out-of-band.
        assert_eq!(order_mutations.len(), 1);
        assert!(order_mutations[0].2.is_none(), "vault maker fully filled");

        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(5));
        assert_eq!(
            pos.entry_price,
            UsdPrice::new_int(50_000),
            "taker filled against the out-of-band vault price"
        );
    }

    /// Cancel–fill–cancel across a single walk. The walk encounters:
    ///
    ///   1. An out-of-band maker at the near end (below lower bound) →
    ///      drift-cancel, `continue`.
    ///   2. An in-band maker → fill.
    ///   3. Another out-of-band maker at the far end (above upper bound) →
    ///      drift-cancel, `continue`.
    ///
    /// This exercises the `continue` semantics twice within one match and
    /// confirms that a mid-walk fill does not mask subsequent drifted
    /// makers. `break` would fail this test: stopping at step 1 would
    /// leave no fill at all.
    #[test]
    fn drift_cancel_fill_cancel_within_one_walk() {
        const MAKER_C: Addr = Addr::mock(4);

        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());
        setup_storage(&mut ctx.storage);

        // Oracle = $30,000, band = 50% → allowed range [$15,000, $45,000].
        //
        // MAKER_A ask at $5,000 — below lower bound (out-of-band, near end).
        // MAKER_B ask at $40,000 — in-band.
        // MAKER_C ask at $50,000 — above upper bound (out-of-band, far end).
        place_ask(&mut ctx.storage, MAKER_A, 5_000, 5, 100);
        place_ask(&mut ctx.storage, MAKER_B, 40_000, 5, 101);
        place_ask(&mut ctx.storage, MAKER_C, 50_000, 5, 102);

        let maker_a_reserved_before = USER_STATES
            .load(&ctx.storage, MAKER_A)
            .unwrap()
            .reserved_margin;
        let maker_c_reserved_before = USER_STATES
            .load(&ctx.storage, MAKER_C)
            .unwrap()
            .reserved_margin;

        let param = test_param();
        let pair_param = test_pair_param();
        // Model an oracle that has moved to $30k; stale makers from when it was
        // higher now fall outside the band around it.
        let pair_state = PairState {
            oracle_price: UsdPrice::new_int(30_000),
            ..PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap()
        };
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(30_000),
            Quantity::new_int(15),
            OrderKind::Market {
                // 99% slippage → target ≈ $59.7k, covers all three asks
                // so that none of them break the walk on target_price
                // before the band check fires.
                max_slippage: Dimensionless::new_percent(99),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Three order_mutations, all removals (None):
        //   [0] MAKER_A cancelled (drift, near-end)
        //   [1] MAKER_B fully filled
        //   [2] MAKER_C cancelled (drift, far-end)
        assert_eq!(order_mutations.len(), 3);
        assert!(order_mutations[0].2.is_none(), "MAKER_A cancelled");
        assert!(order_mutations[1].2.is_none(), "MAKER_B fully filled");
        assert!(order_mutations[2].2.is_none(), "MAKER_C cancelled");

        // Both cancelled makers have their reserved margin released and
        // open_order_count decremented.
        assert_eq!(maker_states[&MAKER_A].open_order_count, 0);
        assert!(
            maker_a_reserved_before.is_non_zero()
                && maker_states[&MAKER_A].reserved_margin < maker_a_reserved_before,
            "MAKER_A reserved margin released"
        );
        assert_eq!(maker_states[&MAKER_C].open_order_count, 0);
        assert!(
            maker_c_reserved_before.is_non_zero()
                && maker_states[&MAKER_C].reserved_margin < maker_c_reserved_before,
            "MAKER_C reserved margin released"
        );

        // MAKER_B was filled normally.
        assert_eq!(maker_states[&MAKER_B].open_order_count, 0);

        // Taker filled exactly 5 @ $40,000 (the single in-band maker).
        let pos = taker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(5));
        assert_eq!(pos.entry_price, UsdPrice::new_int(40_000));
    }

    // ======= Vault-as-maker PnL settlement ===================================

    /// Helper: run a two-step trade where the vault (CONTRACT) is the maker on
    /// both legs.
    ///
    /// Step 1: vault ask at `open_price` matched by taker buy → vault opens short.
    /// Step 2: vault bid at `close_price` matched by taker sell → vault closes short.
    ///
    /// Mutates: nothing persisted.
    ///
    /// Returns: the vault's margin (from its `UserState`) after both trades.
    fn vault_maker_round_trip(
        initial_vault_margin: UsdValue,
        open_price: i128,
        close_price: i128,
        size: i128,
    ) -> UsdValue {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        // Seed vault UserState with the initial margin.
        let vault_init = UserState {
            margin: initial_vault_margin,
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, CONTRACT, &vault_init)
            .unwrap();

        // ---- Step 1: vault places ask, taker buys → vault opens short ----

        place_ask(&mut ctx.storage, CONTRACT, open_price, size, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            pair_state,
            taker_state,
            maker_states,
            order_mutations,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(open_price),
            Quantity::new_int(size),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Persist side-effects so step 2 can load them.
        PAIR_STATES
            .save(&mut ctx.storage, &pair_id(), &pair_state)
            .unwrap();
        USER_STATES
            .save(&mut ctx.storage, TAKER, &taker_state)
            .unwrap();
        for (addr, ms) in &maker_states {
            USER_STATES.save(&mut ctx.storage, *addr, ms).unwrap();
        }
        for (stored_price, order_id, mutation, _) in order_mutations {
            let key = (pair_id(), stored_price, order_id);
            match mutation {
                Some(order) => ASKS.save(&mut ctx.storage, key, &order).unwrap(),
                None => ASKS.remove(&mut ctx.storage, key).unwrap(),
            }
        }

        // Vault must now have a short position.
        let vault_state = USER_STATES.load(&ctx.storage, CONTRACT).unwrap();
        let vault_pos = vault_state.positions.get(&pair_id()).unwrap();
        assert_eq!(vault_pos.size, Quantity::new_int(-size.abs()));

        // ---- Step 2: vault places bid, taker sells → vault closes short ----

        place_bid(&mut ctx.storage, CONTRACT, close_price, size, 200);

        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = USER_STATES.load(&ctx.storage, TAKER).unwrap();

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(close_price),
            Quantity::new_int(-size),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        maker_states[&CONTRACT].margin
    }

    /// Vault opens short at $50,000 then closes at $49,000 → profit of $10,000.
    ///
    /// Vault margin changes from: vault PnL + non-vault fees only.
    /// Non-vault PnL (taker's loss) does NOT change vault margin — the
    /// losing counterparty's margin is adjusted internally.
    #[test]
    fn vault_maker_realizes_profit() {
        let initial_margin = UsdValue::new_int(100_000);

        let vault_margin = vault_maker_round_trip(initial_margin, 50_000, 49_000, 10);

        // Vault margin tracks vault PnL + fee flows only:
        //
        // Step 1 (open at $50,000):
        //   taker fee = |10| × $50,000 × 0.001 = $500 → vault margin += $500
        //
        // Step 2 (close at $49,000):
        //   vault PnL = +$10,000 → vault margin += $10,000
        //   taker fee = $490 → vault margin += $490
        //   taker PnL = -$10,000 → taker margin adjusted (no vault margin change)
        //
        // Total Δ = $500 + $10,000 + $490 = $10,990
        assert_eq!(vault_margin, UsdValue::new_int(110_990));
    }

    /// Vault opens short at $50,000 then closes at $51,000 → loss of $10,000.
    /// Vault margin is large enough to absorb the loss entirely.
    #[test]
    fn vault_maker_realizes_loss_no_bad_debt() {
        let initial_margin = UsdValue::new_int(100_000);

        let vault_margin = vault_maker_round_trip(initial_margin, 50_000, 51_000, 10);

        // Vault margin tracks vault PnL + fee flows only:
        //
        // Step 1 (open at $50,000):
        //   taker fee = $500 → vault margin += $500
        //
        // Step 2 (close at $51,000):
        //   taker fee = $510 → vault margin += $510
        //   vault PnL = -$10,000 → vault margin -= $10,000
        //   taker PnL = +$10,000 → taker margin adjusted (no vault margin change)
        //
        // Total Δ = $500 + $510 - $10,000 = -$8,990
        assert_eq!(vault_margin, UsdValue::new_int(91_010));
    }

    /// Vault has an existing short position at $50,000. A new taker (MAKER_B)
    /// sells against the vault's bid at $51,000, closing the vault's short at a
    /// loss. Vault margin is only $1,000 — not enough to cover the $10,000 loss,
    /// so vault margin goes negative (representing the deficit).
    ///
    /// Fees are collected first (augmenting vault margin), then the vault loss
    /// is applied.
    #[test]
    fn vault_maker_realizes_loss_with_bad_debt() {
        let mut ctx = MockContext::new()
            .with_sender(MAKER_B)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        // Manually set up vault with a short position: -10 @ $50,000
        // and $1,000 margin.
        let mut vault_state = UserState {
            margin: UsdValue::new_int(1_000),
            ..Default::default()
        };
        vault_state.positions.insert(
            pair_id(),
            Position {
                size: Quantity::new_int(-10),
                entry_price: UsdPrice::new_int(50_000),
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );
        USER_STATES
            .save(&mut ctx.storage, CONTRACT, &vault_state)
            .unwrap();

        // Reflect existing OI from the vault's short.
        let mut pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        pair_state.short_oi = Quantity::new_int(10);
        PAIR_STATES
            .save(&mut ctx.storage, &pair_id(), &pair_state)
            .unwrap();

        // Vault places a resting bid at $51,000.
        place_bid(&mut ctx.storage, CONTRACT, 51_000, 10, 200);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        // MAKER_B sells -10 → matches vault bid at $51,000.
        //   vault: closes short at $51,000 → loss = -$10,000
        //   MAKER_B: opens new short → PnL = 0, fee = |10| × $51,000 × 0.001 = $510
        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            MAKER_B,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(51_000),
            Quantity::new_int(-10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Fee loop first: MAKER_B fee = $510 → vault margin += $510
        //   vault margin = $1,000 + $510 = $1,510
        //
        // PnL loop: vault loss = $10,000
        //   vault margin = $1,510 - $10,000 = -$8,490
        assert_eq!(maker_states[&CONTRACT].margin, UsdValue::new_int(-8_490));
    }

    // ===================== Regression: phantom order IDs =====================

    /// Previously, `NEXT_ORDER_ID` was only incremented when an order entered
    /// the book (GTC remainder or post-only). Market orders and limit orders
    /// that were fully filled immediately used a "phantom" order ID: the
    /// value of `NEXT_ORDER_ID` appeared in their `OrderFilled` events but
    /// was never actually consumed, so a subsequent order could reuse the
    /// same ID.
    ///
    /// After the fix, `compute_submit_order_outcome` always increments the order ID counter,
    /// regardless of whether the order enters the book.
    #[test]
    fn orders_not_entering_book_should_increment_next_order_id() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let pair_param = test_pair_param();

        // Case 1: Market order (fully filled, nothing enters book).
        {
            place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

            let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
            let taker_state = UserState {
                margin: LARGE_COLLATERAL,
                ..Default::default()
            };

            let SubmitOrderOutcome {
                pair_state,
                taker_state: _,
                order_to_store,
                next_order_id,
                ..
            } = compute_submit_order_outcome(
                &ctx.storage,
                TAKER,
                CONTRACT,
                Timestamp::ZERO,
                &param,
                &State::default(),
                &pair_id(),
                &pair_param,
                &pair_state,
                &taker_state,
                UsdPrice::new_int(50_000),
                Quantity::new_int(10),
                OrderKind::Market {
                    max_slippage: Dimensionless::new_permille(100),
                },
                false,
                None,
                None,
                &mut EventBuilder::new(),
            )
            .unwrap();

            assert!(
                order_to_store.is_none(),
                "market order should not enter book"
            );
            assert_eq!(
                next_order_id,
                OrderId::new(2),
                "next_order_id must advance even when order doesn't enter book"
            );

            // Persist for case 2.
            NEXT_ORDER_ID
                .save(&mut ctx.storage, &next_order_id)
                .unwrap();
            PAIR_STATES
                .save(&mut ctx.storage, &pair_id(), &pair_state)
                .unwrap();

            // Clean up the filled ask.
            ASKS.remove(
                &mut ctx.storage,
                (pair_id(), UsdPrice::new_int(50_000), Uint64::new(100)),
            )
            .unwrap();

            // Place a fresh ask for case 2.
            place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 200);
        }

        // Case 2: Limit order that fully fills (nothing enters book).
        {
            let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
            let taker_state = UserState {
                margin: LARGE_COLLATERAL,
                ..Default::default()
            };

            let SubmitOrderOutcome {
                pair_state: _,
                taker_state: _,
                order_to_store,
                next_order_id,
                ..
            } = compute_submit_order_outcome(
                &ctx.storage,
                TAKER,
                CONTRACT,
                Timestamp::ZERO,
                &param,
                &State::default(),
                &pair_id(),
                &pair_param,
                &pair_state,
                &taker_state,
                UsdPrice::new_int(50_000),
                Quantity::new_int(10),
                OrderKind::Limit {
                    limit_price: UsdPrice::new_int(50_000),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: None,
                },
                false,
                None,
                None,
                &mut EventBuilder::new(),
            )
            .unwrap();

            assert!(
                order_to_store.is_none(),
                "fully-filled limit should not enter book"
            );
            assert_eq!(
                next_order_id,
                OrderId::new(3),
                "next_order_id must advance again"
            );
        }
    }

    // ==================== Child order tests ====================

    fn make_tp(trigger_price: i128) -> Option<ChildOrder> {
        Some(ChildOrder {
            trigger_price: UsdPrice::new_int(trigger_price),
            max_slippage: Dimensionless::new_percent(1),
            size: None,
        })
    }

    fn make_sl(trigger_price: i128) -> Option<ChildOrder> {
        Some(ChildOrder {
            trigger_price: UsdPrice::new_int(trigger_price),
            max_slippage: Dimensionless::new_percent(2),
            size: None,
        })
    }

    /// Market buy with TP/SL → long position gets above (TP) and below (SL).
    #[test]
    fn co1_market_buy_applies_tp_sl() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            taker_state: ts, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            make_tp(55_000),
            make_sl(45_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        let pos = ts.positions.get(&pair_id()).unwrap();
        assert!(pos.size.is_positive(), "should be long");

        // TP → Above for long
        let above = pos.conditional_order_above.as_ref().unwrap();
        assert_eq!(above.trigger_price, UsdPrice::new_int(55_000));

        // SL → Below for long
        let below = pos.conditional_order_below.as_ref().unwrap();
        assert_eq!(below.trigger_price, UsdPrice::new_int(45_000));
    }

    /// Market sell with TP/SL → short position gets below (TP) and above (SL).
    #[test]
    fn co2_market_sell_applies_tp_sl() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);
        place_bid(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            taker_state: ts, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            make_tp(45_000),
            make_sl(55_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        let pos = ts.positions.get(&pair_id()).unwrap();
        assert!(pos.size.is_negative(), "should be short");

        // TP → Below for short
        let below = pos.conditional_order_below.as_ref().unwrap();
        assert_eq!(below.trigger_price, UsdPrice::new_int(45_000));

        // SL → Above for short
        let above = pos.conditional_order_above.as_ref().unwrap();
        assert_eq!(above.trigger_price, UsdPrice::new_int(55_000));
    }

    /// Market order that fully closes position → TP/SL dropped (position gone).
    #[test]
    fn co3_ignored_on_full_close() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);

        // Give taker an existing long position.
        let mut ts = taker_state(&ctx.storage);
        ts.positions.insert(
            pair_id(),
            Position {
                size: Quantity::new_int(10),
                entry_price: UsdPrice::new_int(50_000),
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );
        USER_STATES.save(&mut ctx.storage, TAKER, &ts).unwrap();

        // Place bid to absorb the sell.
        place_bid(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        // Sell 10 (closes the long). TP/SL should be dropped.
        let pair_state = PairState {
            long_oi: Quantity::new_int(10),
            oracle_price: UsdPrice::new_int(50_000),
            ..Default::default()
        };

        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            taker_state: ts, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &pair_state,
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            make_tp(45_000),
            make_sl(55_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        assert!(
            !ts.positions.contains_key(&pair_id()),
            "position should be closed"
        );
    }

    /// Order with only TP (no SL) → above is set, below is cleared.
    #[test]
    fn co4_tp_only_clears_sl() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            taker_state: ts, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            make_tp(55_000),
            None, // no SL
            &mut EventBuilder::new(),
        )
        .unwrap();

        let pos = ts.positions.get(&pair_id()).unwrap();
        assert!(pos.conditional_order_above.is_some(), "TP should be set");
        assert!(pos.conditional_order_below.is_none(), "SL should be None");
    }

    /// Limit order partially fills → TP/SL applied to position AND stored on
    /// resting order.
    #[test]
    fn co5_limit_partial_fill_applies_and_rests() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);

        // Only 5 available on the ask side, taker wants 10.
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 5, 100);

        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            taker_state: ts,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: None,
            },
            false,
            make_tp(55_000),
            make_sl(45_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Position should have TP/SL from partial fill.
        let pos = ts.positions.get(&pair_id()).unwrap();
        assert!(pos.conditional_order_above.is_some());
        assert!(pos.conditional_order_below.is_some());

        // Resting order should also carry TP/SL.
        let (_, _, resting_order) = order_to_store.expect("should have resting order");
        assert!(resting_order.tp.is_some());
        assert!(resting_order.sl.is_some());
    }

    /// Limit order doesn't fill at all → TP/SL NOT applied to any existing
    /// position, only stored on the resting order.
    #[test]
    fn co6_no_fill_limit_not_applied() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);

        // Give taker an existing long with no conditional orders.
        let mut ts = taker_state(&ctx.storage);
        ts.positions.insert(
            pair_id(),
            Position {
                size: Quantity::new_int(5),
                entry_price: UsdPrice::new_int(50_000),
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: None,
                conditional_order_below: None,
            },
        );
        USER_STATES.save(&mut ctx.storage, TAKER, &ts).unwrap();

        // Place a buy limit at 49_000 — no asks below that so nothing fills.
        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            taker_state: ts,
            order_to_store,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(49_000),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: None,
            },
            false,
            make_tp(55_000),
            make_sl(45_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Position should NOT have conditional orders (no fills).
        let pos = ts.positions.get(&pair_id()).unwrap();
        assert!(pos.conditional_order_above.is_none());
        assert!(pos.conditional_order_below.is_none());

        // Resting order carries TP/SL for later.
        let (_, _, resting_order) = order_to_store.expect("should have resting order");
        assert!(resting_order.tp.is_some());
        assert!(resting_order.sl.is_some());
    }

    /// Maker's resting limit order with TP/SL gets filled → TP/SL applied to
    /// maker's resulting position.
    #[test]
    fn co7_maker_fill_applies_tp_sl() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);

        // Place a maker ask with TP/SL child orders.
        let key = (pair_id(), UsdPrice::new_int(50_000), Uint64::new(100));
        let order = LimitOrder {
            user: MAKER_A,
            size: Quantity::new_int(-10),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(25_000),
            created_at: Timestamp::from_nanos(0),
            tp: make_tp(45_000),
            sl: make_sl(55_000),
            client_order_id: None,
        };
        ASKS.save(&mut ctx.storage, key, &order).unwrap();

        let maker_a_state = UserState {
            margin: LARGE_COLLATERAL,
            open_order_count: 1,
            reserved_margin: UsdValue::new_int(25_000),
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER_A, &maker_a_state)
            .unwrap();

        // Taker buys 10 → fills maker's ask.
        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &taker_state(&ctx.storage),
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Maker should have a short position with TP/SL.
        let maker_state = maker_states.get(&MAKER_A).unwrap();
        let pos = maker_state.positions.get(&pair_id()).unwrap();
        assert!(pos.size.is_negative(), "maker should be short");

        // For shorts: TP → Below, SL → Above
        let below = pos.conditional_order_below.as_ref().unwrap();
        assert_eq!(below.trigger_price, UsdPrice::new_int(45_000));

        let above = pos.conditional_order_above.as_ref().unwrap();
        assert_eq!(above.trigger_price, UsdPrice::new_int(55_000));
    }

    /// Verify next_order_id is incremented correctly: taker order + child orders.
    #[test]
    fn co8_order_id_incremented() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            next_order_id,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            make_tp(55_000),
            make_sl(45_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        // ID 1 = taker order, ID 2 = TP child, ID 3 = SL child → next = 4
        assert_eq!(next_order_id, OrderId::new(4));
    }

    /// Position has existing conditional orders → new order with child orders
    /// overwrites both.
    #[test]
    fn co9_overwrites_existing_conditional_orders() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);

        // Give taker a long position with existing TP/SL.
        let mut ts = taker_state(&ctx.storage);
        ts.positions.insert(
            pair_id(),
            Position {
                size: Quantity::new_int(5),
                entry_price: UsdPrice::new_int(50_000),
                entry_funding_per_unit: FundingPerUnit::ZERO,
                conditional_order_above: Some(ConditionalOrder {
                    order_id: Uint64::new(99),
                    size: None,
                    trigger_price: UsdPrice::new_int(60_000),
                    max_slippage: Dimensionless::new_percent(1),
                }),
                conditional_order_below: Some(ConditionalOrder {
                    order_id: Uint64::new(98),
                    size: None,
                    trigger_price: UsdPrice::new_int(40_000),
                    max_slippage: Dimensionless::new_percent(2),
                }),
            },
        );
        USER_STATES.save(&mut ctx.storage, TAKER, &ts).unwrap();

        // Buy more with different TP/SL.
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 5, 100);

        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            taker_state: ts, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                long_oi: Quantity::new_int(5),
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(5),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            make_tp(58_000), // different from existing 60k
            make_sl(42_000), // different from existing 40k
            &mut EventBuilder::new(),
        )
        .unwrap();

        let pos = ts.positions.get(&pair_id()).unwrap();

        let above = pos.conditional_order_above.as_ref().unwrap();
        assert_eq!(above.trigger_price, UsdPrice::new_int(58_000));
        assert_ne!(above.order_id, Uint64::new(99)); // new ID

        let below = pos.conditional_order_below.as_ref().unwrap();
        assert_eq!(below.trigger_price, UsdPrice::new_int(42_000));
        assert_ne!(below.order_id, Uint64::new(98)); // new ID
    }

    /// Taker has a short position but submits a buy with TP/SL (intended for
    /// long). Buy partially closes the short but doesn't flip it → direction
    /// mismatch → TP/SL dropped.
    ///
    /// Wrong behavior: applying TP/SL to the short position, which would
    /// place trigger prices in nonsensical slots (e.g., TP @ $55k as a Below
    /// trigger on a short would fire immediately).
    #[test]
    fn co10_taker_child_order_dropped_on_direction_mismatch() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        // Taker starts with a short position of -10.
        let ts = UserState {
            margin: LARGE_COLLATERAL,
            positions: {
                let mut p = BTreeMap::new();
                p.insert(
                    pair_id(),
                    Position {
                        size: Quantity::new_int(-10),
                        entry_price: UsdPrice::new_int(50_000),
                        entry_funding_per_unit: FundingPerUnit::ZERO,
                        conditional_order_above: None,
                        conditional_order_below: None,
                    },
                );
                p
            },
            ..Default::default()
        };
        USER_STATES.save(&mut ctx.storage, TAKER, &ts).unwrap();

        // Place ask to fill the taker's buy.
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 5, 100);

        let ts = taker_state(&ctx.storage);

        // Buy 5 with TP/SL intended for a long.
        let SubmitOrderOutcome {
            taker_state: ts, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                short_oi: Quantity::new_int(10),
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(5), // buy
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            make_tp(55_000),
            make_sl(45_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Position is still short (-5) → direction mismatch → TP/SL NOT applied.
        let pos = ts.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(-5));
        assert!(pos.conditional_order_above.is_none());
        assert!(pos.conditional_order_below.is_none());
    }

    /// Maker's resting bid (buy) has TP/SL, but maker's position flipped to
    /// short before this fill. The bid partially closes the short but doesn't
    /// flip it → direction mismatch → TP/SL dropped on maker's position.
    #[test]
    fn co11_maker_child_order_dropped_on_direction_mismatch() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);

        // Maker_A has a short position of -10 (was long, got flipped).
        let maker_a_state = UserState {
            margin: LARGE_COLLATERAL,
            open_order_count: 1,
            reserved_margin: UsdValue::new_int(12_500),
            positions: {
                let mut p = BTreeMap::new();
                p.insert(
                    pair_id(),
                    Position {
                        size: Quantity::new_int(-10),
                        entry_price: UsdPrice::new_int(50_000),
                        entry_funding_per_unit: FundingPerUnit::ZERO,
                        conditional_order_above: None,
                        conditional_order_below: None,
                    },
                );
                p
            },
            ..Default::default()
        };
        USER_STATES
            .save(&mut ctx.storage, MAKER_A, &maker_a_state)
            .unwrap();

        // Maker_A's resting bid (buy 5) with TP/SL intended for a long.
        let inverted_price = !UsdPrice::new_int(50_000);
        let key = (pair_id(), inverted_price, Uint64::new(100));
        let order = LimitOrder {
            user: MAKER_A,
            size: Quantity::new_int(5),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(12_500),
            created_at: Timestamp::from_nanos(0),
            tp: make_tp(55_000),
            sl: make_sl(45_000),
            client_order_id: None,
        };
        BIDS.save(&mut ctx.storage, key, &order).unwrap();

        // Taker sells 5 → fills Maker_A's bid.
        let ts = taker_state(&ctx.storage);

        let SubmitOrderOutcome {
            pair_state: _,
            taker_state: _,
            maker_states,
            ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                short_oi: Quantity::new_int(10),
                oracle_price: UsdPrice::new_int(50_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-5), // sell
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Maker_A bought 5, reducing short from -10 to -5. Still short →
        // direction mismatch → TP/SL NOT applied.
        let maker_state = maker_states.get(&MAKER_A).unwrap();
        let pos = maker_state.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.size, Quantity::new_int(-5));
        assert!(pos.conditional_order_above.is_none());
        assert!(pos.conditional_order_below.is_none());
    }

    /// Fill price is below the SL trigger price — the SL condition is already
    /// met at fill time. The SL is still applied; the cron will trigger it on
    /// the next tick.
    ///
    /// This is correct: the user said "close if price < $49k", and the fill
    /// happened at $48k which is already below $49k. Applying the SL (and
    /// letting the cron close it) is consistent with the user's stated intent.
    ///
    /// Wrong behavior: silently dropping the SL because the condition is
    /// already met — this would remove protection the user explicitly
    /// requested.
    #[test]
    fn co12_child_order_applied_even_when_fill_price_below_sl() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_contract(CONTRACT)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        setup_taker(&mut ctx.storage, LARGE_COLLATERAL);

        // Resting ask at $48k — the fill will happen at this price.
        place_ask(&mut ctx.storage, MAKER_A, 48_000, 10, 100);

        let ts = taker_state(&ctx.storage);

        // Market buy 10 with SL @ $49k. The fill price ($48k) is below the
        // SL trigger ($49k), so the SL condition is already met.
        let SubmitOrderOutcome {
            taker_state: ts, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::from_seconds(0),
            &test_param(),
            &State::default(),
            &pair_id(),
            &test_pair_param(),
            &PairState {
                oracle_price: UsdPrice::new_int(48_000),
                ..Default::default()
            },
            &ts,
            UsdPrice::new_int(48_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            make_sl(49_000),
            &mut EventBuilder::new(),
        )
        .unwrap();

        let pos = ts.positions.get(&pair_id()).unwrap();
        assert_eq!(pos.entry_price, UsdPrice::new_int(48_000));

        // SL is applied even though the trigger condition is already met.
        let below = pos.conditional_order_below.as_ref().unwrap();
        assert_eq!(below.trigger_price, UsdPrice::new_int(49_000));
    }

    // ========== Negative price / slippage rejection tests ==========

    /// A market order with negative max_slippage must be rejected early
    /// with a validation error mentioning "max_slippage".
    ///
    /// Wrong behavior: letting the negative slippage flow into target price
    /// computation, where it silently corrupts the price constraint (e.g.
    /// target_price becomes 0 or negative) and may or may not error later
    /// for an unrelated reason.
    #[test]
    fn reject_market_order_negative_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 47_500, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(-50), // -5%
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("max slippage can't be negative");
    }

    /// A limit order with a negative limit_price must be rejected.
    ///
    /// The banding check at Step 0 subsumes the old positivity check: a
    /// negative limit price is always outside any `max_deviation < 1` band
    /// around a positive oracle price.
    #[test]
    fn reject_limit_order_negative_price() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(-1_000),
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("deviates too far");
    }

    /// A limit order with zero limit_price must be rejected.
    ///
    /// Same banding subsumption as the negative-price case: zero is outside
    /// any legal band around a positive oracle price.
    #[test]
    fn reject_limit_order_zero_price() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::ZERO,
                time_in_force: TimeInForce::PostOnly,
                client_order_id: None,
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("deviates too far");
    }

    /// TP child order with negative trigger_price must be rejected.
    #[test]
    fn reject_tp_negative_trigger_price() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            Some(ChildOrder {
                trigger_price: UsdPrice::new_int(-1_000),
                max_slippage: Dimensionless::new_percent(1),
                size: None,
            }),
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("price must be positive");
    }

    /// SL child order with negative trigger_price must be rejected.
    #[test]
    fn reject_sl_negative_trigger_price() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            Some(ChildOrder {
                trigger_price: UsdPrice::new_int(-1_000),
                max_slippage: Dimensionless::new_percent(1),
                size: None,
            }),
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("price must be positive");
    }

    /// TP child order with negative max_slippage must be rejected.
    #[test]
    fn reject_tp_negative_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            Some(ChildOrder {
                trigger_price: UsdPrice::new_int(60_000),
                max_slippage: Dimensionless::new_int(-1),
                size: None,
            }),
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("max slippage can't be negative");
    }

    /// SL child order with negative max_slippage must be rejected.
    #[test]
    fn reject_sl_negative_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            None,
            Some(ChildOrder {
                trigger_price: UsdPrice::new_int(40_000),
                max_slippage: Dimensionless::new_int(-1),
                size: None,
            }),
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("max slippage can't be negative");
    }

    /// TP child order with 100% max_slippage must be rejected.
    #[test]
    fn reject_tp_100pct_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(100),
            },
            false,
            Some(ChildOrder {
                trigger_price: UsdPrice::new_int(60_000),
                max_slippage: Dimensionless::new_percent(100),
                size: None,
            }),
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("max slippage must be less than 1, got");
    }

    /// SL child order with 150% max_slippage must be rejected.
    #[test]
    fn reject_sl_100pct_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_permille(150),
            },
            false,
            None,
            Some(ChildOrder {
                trigger_price: UsdPrice::new_int(40_000),
                max_slippage: Dimensionless::new_percent(150),
                size: None,
            }),
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("max slippage must be less than 1, got");
    }

    // ============ max_slippage >= 100% must be rejected =======================

    #[test]
    fn reject_market_order_with_100pct_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_percent(100),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("max slippage must be less than 1, got");
    }

    #[test]
    fn reject_market_order_with_150pct_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);
        place_ask(&mut ctx.storage, MAKER_A, 50_000, 10, 100);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(-10), // sell order
            OrderKind::Market {
                max_slippage: Dimensionless::new_percent(150),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("max slippage must be less than 1, got");
    }

    /// Helper: load taker state (returns default if missing).
    fn taker_state(storage: &dyn Storage) -> UserState {
        USER_STATES
            .may_load(storage, TAKER)
            .unwrap()
            .unwrap_or_default()
    }

    fn setup_taker(storage: &mut dyn Storage, margin: UsdValue) {
        let ts = UserState {
            margin,
            ..Default::default()
        };
        USER_STATES.save(storage, TAKER, &ts).unwrap();
    }

    // ======================== client_order_id tests ==========================

    /// A GTC limit order carrying a `client_order_id` is reachable via the
    /// `(sender, cid)` index and the stored `LimitOrder.client_order_id`
    /// matches the submitted value.
    #[test]
    fn submit_limit_with_client_order_id_indexes_it() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };
        let cid = Uint64::new(7);

        let SubmitOrderOutcome { order_to_store, .. } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(49_000),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: Some(cid),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        let (stored_price, order_id, order) = order_to_store.unwrap();
        assert_eq!(order.client_order_id, Some(cid));

        // Apply the outcome: persist the resting order. The new
        // `client_order_id` index entry is written by `IndexedMap::save`.
        BIDS.save(
            &mut ctx.storage,
            (pair_id(), stored_price, order_id),
            &order,
        )
        .unwrap();

        let key = BIDS
            .idx
            .client_order_id
            .may_load_key(&ctx.storage, (TAKER, cid))
            .unwrap();
        assert_eq!(key, Some((pair_id(), stored_price, order_id)));
    }

    /// IOC limit orders never enter the book, so a `client_order_id` is
    /// meaningless — `compute_submit_order_outcome` rejects with a clear error.
    #[test]
    fn submit_ioc_with_client_order_id_rejects() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(49_000),
                time_in_force: TimeInForce::ImmediateOrCancel,
                client_order_id: Some(Uint64::new(7)),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .should_fail_with_error("client_order_id is not allowed");
    }

    /// Submitting a second resting order with a `client_order_id` already
    /// owned by the same sender is rejected by the `UniqueIndex` (returns
    /// `StdError::duplicate_data`).
    #[test]
    fn submit_duplicate_client_order_id_rejects() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let cid = Uint64::new(7);

        let order_a = LimitOrder {
            user: TAKER,
            size: Quantity::new_int(5),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(12_500),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: Some(cid),
        };
        BIDS.save(
            &mut ctx.storage,
            (pair_id(), !UsdPrice::new_int(49_000), Uint64::new(1)),
            &order_a,
        )
        .unwrap();

        // A second order with the same (TAKER, cid) — different price/id, so
        // the primary key differs, but the `client_order_id` index collides.
        let order_b = LimitOrder {
            user: TAKER,
            size: Quantity::new_int(5),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(12_500),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: Some(cid),
        };
        let err = BIDS
            .save(
                &mut ctx.storage,
                (pair_id(), !UsdPrice::new_int(48_000), Uint64::new(2)),
                &order_b,
            )
            .unwrap_err();
        assert!(
            format!("{err:?}").to_lowercase().contains("duplicate"),
            "expected duplicate_data error, got: {err:?}"
        );
    }

    /// On a fill, both the taker's and maker's `OrderFilled` events
    /// surface the respective `client_order_id` from the originally
    /// submitted order, so off-chain consumers can correlate fills with
    /// the cid the trader assigned.
    #[test]
    fn order_filled_events_carry_client_order_ids() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let maker_cid = Uint64::new(11);
        let taker_cid = Uint64::new(22);

        // Maker ask carrying a cid.
        let maker_order = LimitOrder {
            user: MAKER_A,
            size: Quantity::new_int(-10),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(25_000),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: Some(maker_cid),
        };
        ASKS.save(
            &mut ctx.storage,
            (pair_id(), UsdPrice::new_int(50_000), Uint64::new(100)),
            &maker_order,
        )
        .unwrap();
        USER_STATES
            .save(
                &mut ctx.storage,
                MAKER_A,
                &UserState {
                    margin: LARGE_COLLATERAL,
                    open_order_count: 1,
                    reserved_margin: UsdValue::new_int(25_000),
                    ..Default::default()
                },
            )
            .unwrap();

        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };
        let mut events = EventBuilder::new();

        // Taker GTC limit buy carrying its own cid fully fills the maker.
        compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Limit {
                limit_price: UsdPrice::new_int(50_000),
                time_in_force: TimeInForce::GoodTilCanceled,
                client_order_id: Some(taker_cid),
            },
            false,
            None,
            None,
            &mut events,
        )
        .unwrap();

        let order_filleds: Vec<OrderFilled> = events
            .into_iter()
            .filter(|e| e.ty == OrderFilled::EVENT_NAME)
            .map(|e| e.data.deserialize_json().unwrap())
            .collect();

        assert_eq!(order_filleds.len(), 2, "expected one OrderFilled per side");

        let taker_filled = order_filleds
            .iter()
            .find(|f| f.user == TAKER)
            .expect("taker OrderFilled missing");
        assert_eq!(taker_filled.client_order_id, Some(taker_cid));

        let maker_filled = order_filleds
            .iter()
            .find(|f| f.user == MAKER_A)
            .expect("maker OrderFilled missing");
        assert_eq!(maker_filled.client_order_id, Some(maker_cid));
    }

    /// When a maker order with a `client_order_id` is fully filled and
    /// removed from the book, the `(sender, cid)` index entry is cleared
    /// automatically by `IndexedMap::remove`.
    #[test]
    fn client_order_id_alias_clears_on_fill() {
        let mut ctx = MockContext::new()
            .with_sender(TAKER)
            .with_funds(Coins::default());

        setup_storage(&mut ctx.storage);

        let cid = Uint64::new(7);

        // Place a maker ask owned by MAKER_A with cid=Some(7).
        let maker_order = LimitOrder {
            user: MAKER_A,
            size: Quantity::new_int(-10),
            reduce_only: false,
            reserved_margin: UsdValue::new_int(25_000),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: Some(cid),
        };
        let maker_key = (pair_id(), UsdPrice::new_int(50_000), Uint64::new(100));
        ASKS.save(&mut ctx.storage, maker_key, &maker_order)
            .unwrap();
        USER_STATES
            .save(
                &mut ctx.storage,
                MAKER_A,
                &UserState {
                    margin: LARGE_COLLATERAL,
                    open_order_count: 1,
                    reserved_margin: UsdValue::new_int(25_000),
                    ..Default::default()
                },
            )
            .unwrap();

        // Sanity: the alias is reachable before the fill.
        assert!(
            ASKS.idx
                .client_order_id
                .may_load_key(&ctx.storage, (MAKER_A, cid))
                .unwrap()
                .is_some()
        );

        // Taker fully fills the maker.
        let param = test_param();
        let pair_param = test_pair_param();
        let pair_state = PAIR_STATES.load(&ctx.storage, &pair_id()).unwrap();
        let taker_state = UserState {
            margin: LARGE_COLLATERAL,
            ..Default::default()
        };

        let SubmitOrderOutcome {
            order_mutations, ..
        } = compute_submit_order_outcome(
            &ctx.storage,
            TAKER,
            CONTRACT,
            Timestamp::ZERO,
            &param,
            &State::default(),
            &pair_id(),
            &pair_param,
            &pair_state,
            &taker_state,
            UsdPrice::new_int(50_000),
            Quantity::new_int(10),
            OrderKind::Market {
                max_slippage: Dimensionless::new_percent(10),
            },
            false,
            None,
            None,
            &mut EventBuilder::new(),
        )
        .unwrap();

        // Apply the order mutation: a `None` mutation removes the maker.
        for (stored_price, order_id, mutation, _) in order_mutations {
            let key = (pair_id(), stored_price, order_id);
            match mutation {
                Some(o) => ASKS.save(&mut ctx.storage, key, &o).unwrap(),
                None => ASKS.remove(&mut ctx.storage, key).unwrap(),
            }
        }

        // After full fill, the alias is gone — same `cid` is reusable.
        assert!(
            ASKS.idx
                .client_order_id
                .may_load_key(&ctx.storage, (MAKER_A, cid))
                .unwrap()
                .is_none()
        );
    }
}
