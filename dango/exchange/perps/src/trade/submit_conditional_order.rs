use {
    crate::{PAIR_PARAMS, USER_STATES, state::PARAM, trade::ensure_trading_enabled},
    anyhow::{anyhow, ensure},
    dango_math::NumberConst,
    dango_order_book::{
        ConditionalOrder, ConditionalOrderId, ConditionalOrderPlaced, Dimensionless, NEXT_ORDER_ID,
        PairId, Quantity, TriggerDirection, UsdPrice, validate_slippage,
    },
    dango_primitives::{MutableCtx, Response},
};

pub fn submit_conditional_order(
    ctx: MutableCtx,
    pair_id: PairId,
    size: Option<Quantity>,
    trigger_price: UsdPrice,
    trigger_direction: TriggerDirection,
    max_slippage: Dimensionless,
) -> anyhow::Result<Response> {
    ensure_trading_enabled(&PARAM.load(ctx.storage)?)?;

    let mut user_state = USER_STATES.load(ctx.storage, ctx.sender)?;
    let pair_param = PAIR_PARAMS.load(ctx.storage, &pair_id)?;

    // -------------------------------- Checks ---------------------------------

    ensure!(
        trigger_price.is_positive(),
        "price must be positive: {trigger_price}"
    );

    validate_slippage(max_slippage, pair_param.max_market_slippage)?;

    // 1. User must have an open position in this pair.
    // 2. If size is specified: sign must oppose position, |size| <= |position.size|.
    // 3. Must not already have a conditional order of the same direction for this pair.

    let position = user_state
        .positions
        .get(&pair_id)
        .ok_or_else(|| anyhow!("no position in pair {pair_id}"))?;

    if let Some(size) = size {
        ensure!(
            (size.is_negative() && position.size.is_positive())
                || (size.is_positive() && position.size.is_negative()),
            "size must oppose position direction"
        );
    }

    // ----------------------------- State changes -----------------------------

    // Assign order ID and increment.
    let order_id = NEXT_ORDER_ID.load(ctx.storage)?;
    NEXT_ORDER_ID.save(ctx.storage, &(order_id + ConditionalOrderId::ONE))?;

    let conditional_order = ConditionalOrder {
        order_id,
        size,
        trigger_price,
        max_slippage,
    };

    // Set the field on the position.
    let position = user_state
        .positions
        .get_mut(&pair_id)
        .ok_or_else(|| anyhow!("no position in pair {pair_id}"))?;

    match trigger_direction {
        TriggerDirection::Above => position.conditional_order_above = Some(conditional_order),
        TriggerDirection::Below => position.conditional_order_below = Some(conditional_order),
    }

    USER_STATES.save(ctx.storage, ctx.sender, &user_state)?;

    Ok(Response::new().add_event(ConditionalOrderPlaced {
        pair_id,
        user: ctx.sender,
        trigger_price,
        trigger_direction,
        size,
        max_slippage,
    })?)
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{PAIR_PARAMS, PARAM, USER_STATES},
        dango_math::{NumberConst, Uint64},
        dango_order_book::{
            Dimensionless, FundingPerUnit, NEXT_ORDER_ID, OrderId, Quantity, TriggerDirection,
            UsdPrice, UsdValue,
        },
        dango_primitives::{Addr, Coins, MockContext, ResultExt, Storage},
        dango_types::perps::{PairParam, Param, Position, UserState},
        std::collections::BTreeMap,
    };

    const USER: Addr = Addr::mock(1);

    fn pair_id() -> PairId {
        "perp/ethusd".parse().unwrap()
    }

    fn test_pair_param() -> PairParam {
        PairParam {
            max_market_slippage: Dimensionless::new_permille(100), // 10%
            ..PairParam::new_mock()
        }
    }

    fn long_position(size: i128) -> Position {
        Position {
            size: Quantity::new_int(size),
            entry_price: UsdPrice::new_int(2_000),
            entry_funding_per_unit: FundingPerUnit::ZERO,
            conditional_order_above: None,
            conditional_order_below: None,
        }
    }

    fn short_position(size: i128) -> Position {
        Position {
            size: Quantity::new_int(-size),
            entry_price: UsdPrice::new_int(2_000),
            entry_funding_per_unit: FundingPerUnit::ZERO,
            conditional_order_above: None,
            conditional_order_below: None,
        }
    }

    fn user_state_with_position(position: Position) -> UserState {
        let mut positions = BTreeMap::new();
        positions.insert(pair_id(), position);
        UserState {
            margin: UsdValue::new_int(10_000),
            positions,
            ..Default::default()
        }
    }

    fn init_storage(storage: &mut dyn Storage, user_state: UserState) {
        PARAM.save(storage, &Param::default()).unwrap();
        PAIR_PARAMS
            .save(storage, &pair_id(), &test_pair_param())
            .unwrap();
        NEXT_ORDER_ID.save(storage, &OrderId::ONE).unwrap();
        USER_STATES.save(storage, USER, &user_state).unwrap();
    }

    /// Take-profit on a long position: sell 5 of 10 ETH when price rises
    /// above $2,500.
    ///
    /// Expected: order stored in `conditional_order_above` with correct
    /// trigger price, size, and a freshly allocated order ID. The global
    /// `NEXT_ORDER_ID` counter advances by one.
    #[test]
    fn p1_valid_tp_on_long() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_succeed();

        // Order stored in position's conditional_order_above field.
        let user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        let position = user_state.positions.get(&pair_id()).unwrap();
        assert!(position.conditional_order_above.is_some());

        let order = position.conditional_order_above.as_ref().unwrap();
        assert_eq!(order.order_id, Uint64::ONE);
        assert_eq!(order.size, Some(Quantity::new_int(-5)));
        assert_eq!(order.trigger_price, UsdPrice::new_int(2_500));

        // Next order ID incremented.
        let next_id = NEXT_ORDER_ID.load(&ctx.storage).unwrap();
        assert_eq!(next_id, Uint64::new(2));
    }

    /// Stop-loss on a long position: sell all 10 ETH when price drops
    /// below $1,800.
    ///
    /// Expected: order stored in `conditional_order_below`. Mirrors p1 but
    /// for the opposite trigger direction.
    #[test]
    fn p2_valid_sl_on_long() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-10)),
            UsdPrice::new_int(1_800),
            TriggerDirection::Below,
            Dimensionless::new_percent(2),
        )
        .should_succeed();

        let user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        let position = user_state.positions.get(&pair_id()).unwrap();
        assert!(position.conditional_order_below.is_some());

        let order = position.conditional_order_below.as_ref().unwrap();
        assert_eq!(order.order_id, Uint64::ONE);
    }

    /// Take-profit on a short position: buy 5 of 10 ETH when price drops
    /// below $1,500.
    ///
    /// Expected: order stored in `conditional_order_below`. For shorts, TP
    /// triggers *below* (profit when price falls), which is the opposite of
    /// longs.
    ///
    /// Wrong behavior: storing it in `conditional_order_above` — that would
    /// make it a stop-loss for a short.
    #[test]
    fn p3_valid_tp_on_short() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(short_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(5)),
            UsdPrice::new_int(1_500),
            TriggerDirection::Below,
            Dimensionless::new_percent(1),
        )
        .should_succeed();

        let user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        let position = user_state.positions.get(&pair_id()).unwrap();
        assert!(position.conditional_order_below.is_some());

        let order = position.conditional_order_below.as_ref().unwrap();
        assert_eq!(order.order_id, Uint64::ONE);
    }

    /// Conditional order size must oppose the position direction: a long
    /// position can only have a *sell* (negative size) conditional order.
    ///
    /// Expected: error "size must oppose position direction" when submitting
    /// a positive (buy) size on a long position.
    ///
    /// Wrong behavior: accepting the order — this would let a conditional
    /// order *increase* the position, but conditional orders are always
    /// reduce-only.
    #[test]
    fn p4_reject_wrong_direction() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        // Positive size on a long position = same direction.
        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_fail_with_error("size must oppose position direction");
    }

    /// Conditional order size larger than the position is allowed at
    /// submission time. The cron job clamps the size to the actual position
    /// size when the order triggers.
    ///
    /// Expected: order placed successfully with size = -5 even though the
    /// position is only 3.
    ///
    /// Wrong behavior (old): rejecting with "conditional order size exceeds
    /// position size". This was overly strict because the position size can
    /// change between submission and trigger, and the cron already clamps.
    #[test]
    fn p5_allow_exceeds_position() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(&mut ctx.storage, user_state_with_position(long_position(3)));

        // Size exceeds position — allowed because it's clamped at trigger time.
        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_succeed();

        let user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        let position = user_state.positions.get(&pair_id()).unwrap();
        let order = position.conditional_order_above.as_ref().unwrap();
        assert_eq!(order.size, Some(Quantity::new_int(-5)));
    }

    /// A conditional order requires an existing position — it is meaningless
    /// without one since it's always reduce-only.
    ///
    /// Expected: error "no position" when the user has margin but no open
    /// position in the requested pair.
    ///
    /// Wrong behavior: accepting the order — it would sit on a non-existent
    /// position and never trigger, or worse, trigger against a future
    /// position with unexpected direction.
    #[test]
    fn p6_reject_no_position() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        // User state with no positions.
        let user_state = UserState {
            margin: UsdValue::new_int(10_000),
            ..Default::default()
        };
        init_storage(&mut ctx.storage, user_state);

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_fail_with_error("no position");
    }

    /// Submitting a second conditional order with the same trigger direction
    /// overwrites the first. The other direction is not affected.
    ///
    /// Expected: the second Above order (trigger $3,000, size -3, order_id 2)
    /// replaces the first (trigger $2,500, size -5, order_id 1). A subsequent
    /// Below order coexists without disturbing the Above.
    ///
    /// Wrong behavior (old): rejecting the second order with "conditional
    /// order already exists". This forced users to cancel-then-resubmit,
    /// which is a poor UX and creates a window with no protection.
    #[test]
    fn p7_overwrite_duplicate_direction() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        // First Above order — should succeed.
        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_succeed();

        // Second Above order for same pair — overwrites the first.
        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-3)),
            UsdPrice::new_int(3_000),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_succeed();

        // Verify the overwrite: trigger_price and order_id reflect the new order.
        let user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        let position = user_state.positions.get(&pair_id()).unwrap();
        let order = position.conditional_order_above.as_ref().unwrap();
        assert_eq!(order.order_id, Uint64::new(2));
        assert_eq!(order.trigger_price, UsdPrice::new_int(3_000));
        assert_eq!(order.size, Some(Quantity::new_int(-3)));

        // Below order for same pair — should still succeed (different direction).
        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(1_800),
            TriggerDirection::Below,
            Dimensionless::new_percent(2),
        )
        .should_succeed();

        // Verify Above was NOT affected by the Below submission.
        let user_state = USER_STATES.load(&ctx.storage, USER).unwrap();
        let position = user_state.positions.get(&pair_id()).unwrap();
        assert_eq!(
            position
                .conditional_order_above
                .as_ref()
                .unwrap()
                .trigger_price,
            UsdPrice::new_int(3_000)
        );
    }

    /// A position can hold both an Above and a Below conditional order at
    /// the same time (TP + SL bracket).
    ///
    /// Expected: after submitting TP @ $2,500 (Above) and SL @ $1,800
    /// (Below), both are stored on the position with distinct order IDs
    /// (1 and 2 respectively).
    #[test]
    fn p8_multiple_on_same_pair() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        // TP @ $2,500
        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_succeed();

        // SL @ $1,800
        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-10)),
            UsdPrice::new_int(1_800),
            TriggerDirection::Below,
            Dimensionless::new_percent(2),
        )
        .should_succeed();

        // Both orders exist on the position.
        let user_state = USER_STATES.load(&ctx.storage, USER).unwrap();

        let position = user_state.positions.get(&pair_id()).unwrap();
        assert!(position.conditional_order_above.is_some());

        let above = position.conditional_order_above.as_ref().unwrap();
        assert_eq!(above.order_id, Uint64::ONE);
        assert_eq!(above.trigger_price, UsdPrice::new_int(2_500));

        assert!(position.conditional_order_below.is_some());
        let below = position.conditional_order_below.as_ref().unwrap();
        assert_eq!(below.order_id, Uint64::new(2));
        assert_eq!(below.trigger_price, UsdPrice::new_int(1_800));
    }

    /// Conditional order with negative trigger_price must be rejected.
    ///
    /// Expected: error mentioning that trigger_price must be positive.
    ///
    /// Wrong behavior: accepting the order — a negative trigger price is
    /// nonsensical and could never match a real oracle price.
    #[test]
    fn p9_reject_negative_trigger_price() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(-2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(1),
        )
        .should_fail_with_error("price must be positive");
    }

    /// Conditional order with zero trigger_price must be rejected.
    #[test]
    fn p10_reject_zero_trigger_price() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::ZERO,
            TriggerDirection::Below,
            Dimensionless::new_percent(1),
        )
        .should_fail_with_error("price must be positive");
    }

    /// Conditional order with negative max_slippage must be rejected.
    ///
    /// Expected: error mentioning that max_slippage must be positive.
    ///
    /// Wrong behavior: accepting the order — a negative slippage inverts
    /// the price constraint when the conditional order triggers, causing
    /// fills at arbitrarily bad prices.
    #[test]
    fn p11_reject_negative_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_int(-1),
        )
        .should_fail_with_error("max slippage can't be negative");
    }

    #[test]
    fn p12_reject_100pct_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(100),
        )
        .should_fail_with_error("max slippage must be less than 1, got");
    }

    #[test]
    fn p13_reject_150pct_max_slippage() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_percent(150),
        )
        .should_fail_with_error("max slippage must be less than 1, got");
    }

    /// Conditional order slippage exceeding the pair's `max_market_slippage`
    /// cap is rejected. Test pair has `max_market_slippage = 10%`.
    #[test]
    fn p14_reject_max_slippage_above_pair_cap() {
        let mut ctx = MockContext::new()
            .with_sender(USER)
            .with_funds(Coins::default());

        init_storage(
            &mut ctx.storage,
            user_state_with_position(long_position(10)),
        );

        submit_conditional_order(
            ctx.as_mutable(),
            pair_id(),
            Some(Quantity::new_int(-5)),
            UsdPrice::new_int(2_500),
            TriggerDirection::Above,
            Dimensionless::new_permille(110), // 11% > 10% cap
        )
        .should_fail_with_error("exceeds the pair cap");
    }
}
