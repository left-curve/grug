//! Dynamic re-sizing of a user's resting reduce-only orders.
//!
//! A reduce-only order may only move its owner's position toward zero. This is
//! enforced per-order at placement and re-checked at match time, but a user can
//! rest several reduce-only orders whose sizes individually fit the position yet
//! together exceed it, and the position can shrink or flip after the orders are
//! placed (other fills, liquidation, ADL). This module restores the invariant:
//!
//! > For every (user, pair): the resting reduce-only orders are all on the
//! > position-closing side, and their absolute sizes sum to no more than
//! > `|position|`.
//!
//! It runs after every position change, scanning the user's reduce-only orders
//! and greedily shrinking/cancelling the worst by price-time priority until the
//! sum fits. A closed or flipped position leaves every reduce-only order on the
//! wrong side, so all of them are cancelled.

use {
    super::cancel_order::compute_cancel_one_order_outcome,
    crate::state::{PAIR_PARAMS, USER_STATES},
    dango_order_book::{
        ASKS, BIDS, LimitOrder, OrderId, OrderKey, OrderResized, PairId, Quantity,
        ReasonForOrderRemoval, decrease_liquidity_depths, increase_liquidity_depths,
        may_invert_price,
    },
    dango_primitives::{Addr, EventBuilder, Order as IterationOrder, StdResult, Storage},
    dango_types::perps::UserState,
};

/// Owned outcome of [`compute_resize_reduce_only_outcome`]. Carries the updated
/// `UserState` copy (the caller persists it) and the ids of orders cancelled
/// this pass. Order-book and liquidity-depth writes happen *inside* the function
/// — storage has tx-level rollback, so they need not be deferred — but the
/// caller-persistable `UserState` is taken by `&` and returned owned, per
/// `dango/exchange/perps/purity.md`.
#[derive(Debug)]
pub struct ResizeReduceOnlyOutcome {
    pub user_state: UserState,
    pub removed: Vec<OrderId>,
}

/// Re-clamp `user`'s resting reduce-only orders on `pair_id` to the current
/// position (see the module docs for the invariant). Pure w.r.t. `UserState`:
/// clones it, mutates the clone, and returns the updated copy; writes the order
/// book and liquidity depth inline. The returned `removed` lists the ids
/// cancelled this pass so the placement path can reject a just-rested order that
/// the sum-clamp zeroed.
pub fn compute_resize_reduce_only_outcome(
    storage: &mut dyn Storage,
    user: Addr,
    pair_id: &PairId,
    user_state: &UserState,
    mut events: Option<&mut EventBuilder>,
) -> StdResult<ResizeReduceOnlyOutcome> {
    let mut user_state = user_state.clone();
    let mut removed = Vec::new();

    let position = user_state
        .positions
        .get(pair_id)
        .map(|p| p.size)
        .unwrap_or(Quantity::ZERO);

    // Collect the user's reduce-only orders for this pair from each book. The
    // `user` index ranges in primary-key order — `(pair_id, stored_price,
    // order_id)` — so for each side this is already best-price-time-first: asks
    // ascend by real price; bids are stored at the inverted price, so they
    // ascend by descending real price. We rely on that ordering to allocate the
    // position to the best orders first and shrink the worst.
    let ro_asks = ASKS
        .idx
        .user
        .prefix(user)
        .range(storage, None, None, IterationOrder::Ascending)
        .collect::<StdResult<Vec<_>>>()?
        .into_iter()
        .filter(|(key, order)| &key.0 == pair_id && order.reduce_only)
        .collect::<Vec<(OrderKey, LimitOrder)>>();

    let ro_bids = BIDS
        .idx
        .user
        .prefix(user)
        .range(storage, None, None, IterationOrder::Ascending)
        .collect::<StdResult<Vec<_>>>()?
        .into_iter()
        .filter(|(key, order)| &key.0 == pair_id && order.reduce_only)
        .collect::<Vec<(OrderKey, LimitOrder)>>();

    // The closing side reduces the position toward zero; orders on the other
    // side would grow it, so they are always cancelled. When the position is
    // flat, every reduce-only order is on the wrong side.
    let (closing, wrong): (Vec<_>, Vec<_>) = if position.is_positive() {
        (ro_asks, ro_bids)
    } else if position.is_negative() {
        (ro_bids, ro_asks)
    } else {
        (Vec::new(), ro_asks.into_iter().chain(ro_bids).collect())
    };

    if closing.is_empty() && wrong.is_empty() {
        return Ok(ResizeReduceOnlyOutcome {
            user_state,
            removed,
        });
    }

    let pair_param = PAIR_PARAMS.load(storage, pair_id)?;

    // Cancel every wrong-side order (the position closed or flipped).
    for (order_key, order) in wrong {
        let order_id = order_key.2;
        compute_cancel_one_order_outcome(
            storage,
            &mut user_state,
            order_key,
            order,
            events.as_deref_mut(),
            ReasonForOrderRemoval::ReduceOnlyResized,
            |_, _| Ok(pair_param.clone()),
        )?;
        removed.push(order_id);
    }

    // Walk the closing side best-first, allocating `|position|` as a shared
    // budget. Each order keeps at most what is left; an order that gets nothing
    // is cancelled, and one that gets less than its size is shrunk. Because the
    // budget is shared, it is the *sum* of the orders that is clamped, not each
    // order individually.
    let mut budget = position.checked_abs()?;

    for (order_key, order) in closing {
        let order_id = order_key.2;
        let abs_size = order.size.checked_abs()?;
        let alloc = abs_size.min(budget);
        budget = budget.checked_sub(alloc)?;

        if alloc.is_zero() {
            compute_cancel_one_order_outcome(
                storage,
                &mut user_state,
                order_key,
                order,
                events.as_deref_mut(),
                ReasonForOrderRemoval::ReduceOnlyResized,
                |_, _| Ok(pair_param.clone()),
            )?;
            removed.push(order_id);
        } else if alloc < abs_size {
            // Shrink in place: keep the order's sign, lower only its magnitude.
            let new_size = if order.size.is_negative() {
                alloc.checked_neg()?
            } else {
                alloc
            };

            // Release reserved margin for the removed portion. Derive the
            // release once (the proportion of the size that goes away) and
            // subtract it from *both* the aggregate and the per-order field,
            // mirroring the maker partial-fill in `match_order`. Recomputing the
            // per-order value with its own `× |new| / |old|` division would let
            // two truncating divisions disagree and strand margin.
            let margin_to_release = order
                .reserved_margin
                .checked_mul(order.size.checked_sub(new_size)?)?
                .checked_div(order.size)?;

            user_state
                .reserved_margin
                .checked_sub_assign(margin_to_release)?;

            let is_bid = order.size.is_positive();
            let real_price = may_invert_price(order_key.1, is_bid);

            // Remove the old depth contribution and re-add the new size (rather
            // than adjusting by the delta) to avoid notional drift — see
            // `liquidity_depth.rs::partial_fill_no_residual_depth`.
            decrease_liquidity_depths(
                storage,
                pair_id,
                is_bid,
                real_price,
                abs_size,
                &pair_param.bucket_sizes,
            )?;
            increase_liquidity_depths(
                storage,
                pair_id,
                is_bid,
                real_price,
                alloc,
                &pair_param.bucket_sizes,
            )?;

            let mut updated = order.clone();
            updated.size = new_size;
            updated.reserved_margin = order.reserved_margin.checked_sub(margin_to_release)?;

            let book = if is_bid {
                BIDS
            } else {
                ASKS
            };
            book.save(storage, order_key, &updated)?;

            if let Some(events) = events.as_deref_mut() {
                events.push(OrderResized {
                    order_id,
                    pair_id: pair_id.clone(),
                    user,
                    old_size: order.size,
                    new_size,
                    client_order_id: order.client_order_id,
                })?;
            }
        }
        // else: `alloc == abs_size`, the order fits entirely — leave it as is.
    }

    Ok(ResizeReduceOnlyOutcome {
        user_state,
        removed,
    })
}

/// Persist-layer wrapper over [`compute_resize_reduce_only_outcome`]. Loads the
/// user's state (no-op if the user has none — then there are no orders to
/// re-size), runs the core, and persists the returned state (removing it once it
/// becomes empty). Returns the ids cancelled this pass.
pub fn resize_reduce_only_orders(
    storage: &mut dyn Storage,
    user: Addr,
    pair_id: &PairId,
    events: &mut EventBuilder,
) -> StdResult<Vec<OrderId>> {
    let Some(user_state) = USER_STATES.may_load(storage, user)? else {
        return Ok(Vec::new());
    };

    let ResizeReduceOnlyOutcome {
        user_state,
        removed,
    } = compute_resize_reduce_only_outcome(storage, user, pair_id, &user_state, Some(events))?;

    if user_state.is_empty() {
        USER_STATES.remove(storage, user)?;
    } else {
        USER_STATES.save(storage, user, &user_state)?;
    }

    Ok(removed)
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_math::Uint64,
        dango_order_book::{FundingPerUnit, UsdPrice, UsdValue},
        dango_primitives::{MockContext, Timestamp},
        dango_types::perps::{PairParam, Position},
        std::collections::{BTreeMap, VecDeque},
    };

    const USER: Addr = Addr::mock(1);

    fn pair() -> PairId {
        "perp/btcusd".parse().unwrap()
    }

    fn other_pair() -> PairId {
        "perp/ethusd".parse().unwrap()
    }

    fn ensure_pair_param(storage: &mut dyn Storage, pair: &PairId) {
        if PAIR_PARAMS.may_load(storage, pair).unwrap().is_none() {
            PAIR_PARAMS
                .save(storage, pair, &PairParam::default())
                .unwrap();
        }
    }

    /// Save a reduce-only ask (negative size) at a real `price`. For asks the
    /// stored key price equals the real price.
    fn save_ro_ask(
        storage: &mut dyn Storage,
        pair: &PairId,
        order_id: u64,
        price: i128,
        size: i128,
        reserved: i128,
    ) {
        ensure_pair_param(storage, pair);
        let key = (
            pair.clone(),
            UsdPrice::new_int(price),
            Uint64::new(order_id),
        );
        let order = LimitOrder {
            user: USER,
            size: Quantity::new_int(size),
            reduce_only: true,
            reserved_margin: UsdValue::new_int(reserved),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        ASKS.save(storage, key, &order).unwrap();
    }

    /// Save a reduce-only bid (positive size) at a real `price`. For bids the
    /// stored key price is inverted, matching `store_limit_order`.
    fn save_ro_bid(
        storage: &mut dyn Storage,
        pair: &PairId,
        order_id: u64,
        price: i128,
        size: i128,
        reserved: i128,
    ) {
        ensure_pair_param(storage, pair);
        let key = (
            pair.clone(),
            may_invert_price(UsdPrice::new_int(price), true),
            Uint64::new(order_id),
        );
        let order = LimitOrder {
            user: USER,
            size: Quantity::new_int(size),
            reduce_only: true,
            reserved_margin: UsdValue::new_int(reserved),
            created_at: Timestamp::from_nanos(0),
            tp: None,
            sl: None,
            client_order_id: None,
        };
        BIDS.save(storage, key, &order).unwrap();
    }

    fn load_ask(
        storage: &dyn Storage,
        pair: &PairId,
        price: i128,
        order_id: u64,
    ) -> Option<LimitOrder> {
        ASKS.may_load(
            storage,
            (
                pair.clone(),
                UsdPrice::new_int(price),
                Uint64::new(order_id),
            ),
        )
        .unwrap()
    }

    fn load_bid(
        storage: &dyn Storage,
        pair: &PairId,
        price: i128,
        order_id: u64,
    ) -> Option<LimitOrder> {
        BIDS.may_load(
            storage,
            (
                pair.clone(),
                may_invert_price(UsdPrice::new_int(price), true),
                Uint64::new(order_id),
            ),
        )
        .unwrap()
    }

    /// Build a `UserState` carrying a single position (size, in `pair()`) plus
    /// the given reserved margin and open-order count. A zero `position` leaves
    /// the user flat (no position entry).
    fn user_state_with(position: i128, reserved: i128, open_orders: usize) -> UserState {
        let mut positions = BTreeMap::new();
        if position != 0 {
            positions.insert(
                pair(),
                Position {
                    size: Quantity::new_int(position),
                    entry_price: UsdPrice::new_int(2_000),
                    entry_funding_per_unit: FundingPerUnit::ZERO,
                    conditional_order_above: None,
                    conditional_order_below: None,
                },
            );
        }
        UserState {
            unlocks: VecDeque::new(),
            positions,
            reserved_margin: UsdValue::new_int(reserved),
            open_order_count: open_orders,
            ..Default::default()
        }
    }

    /// Sum of the reserved margin of every reduce-only order the user has
    /// resting across both books — used to assert nothing is orphaned.
    fn total_resting_reserved(storage: &dyn Storage) -> UsdValue {
        let mut total = UsdValue::ZERO;
        for res in ASKS
            .idx
            .user
            .prefix(USER)
            .range(storage, None, None, IterationOrder::Ascending)
        {
            total
                .checked_add_assign(res.unwrap().1.reserved_margin)
                .unwrap();
        }
        for res in BIDS
            .idx
            .user
            .prefix(USER)
            .range(storage, None, None, IterationOrder::Ascending)
        {
            total
                .checked_add_assign(res.unwrap().1.reserved_margin)
                .unwrap();
        }
        total
    }

    /// Aggregate reduce-only size already fits the position ⇒ everything is left
    /// untouched and nothing is cancelled.
    #[test]
    fn noop_when_within_budget() {
        let mut ctx = MockContext::new();

        // Long 10; two reduce-only sells summing to 7 ≤ 10.
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_002, -4, 40);
        let user_state = user_state_with(10, 70, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::new_int(70));
        assert_eq!(outcome.user_state.open_order_count, 2);
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_001, 1).unwrap().size,
            Quantity::new_int(-3)
        );
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_002, 2).unwrap().size,
            Quantity::new_int(-4)
        );
    }

    /// Two reduce-only sells that each individually fit the position but together
    /// exceed it: the budget clamps the *sum*, shrinking the worst (newest /
    /// higher-priced) order. Also asserts margin conservation.
    #[test]
    fn shrink_worst_first_clamps_the_sum() {
        let mut ctx = MockContext::new();

        // Long 4; two reduce-only sells of 3 each (each ≤ 4, together 6 > 4).
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_002, -3, 30);
        let user_state = user_state_with(4, 60, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        // @2001 (best) keeps its 3; @2002 shrinks to 1 (budget 4 − 3 = 1).
        assert!(outcome.removed.is_empty());
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_001, 1).unwrap().size,
            Quantity::new_int(-3)
        );
        let shrunk = load_ask(&ctx.storage, &pair(), 2_002, 2).unwrap();
        assert_eq!(shrunk.size, Quantity::new_int(-1));
        // Reserved released proportionally: 30 × 2/3 = 20, leaving 10.
        assert_eq!(shrunk.reserved_margin, UsdValue::new_int(10));
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::new_int(40));
        // Nothing orphaned: aggregate == Σ of resting orders' reserved.
        assert_eq!(
            outcome.user_state.reserved_margin,
            total_resting_reserved(&ctx.storage)
        );
        // Sum of resting sizes equals the position.
        assert_eq!(outcome.user_state.open_order_count, 2);
    }

    /// An order beyond the remaining budget is cancelled entirely (id reported in
    /// `removed`, reserved fully released, count decremented).
    #[test]
    fn cancel_on_zero_budget() {
        let mut ctx = MockContext::new();

        // Long 3; two reduce-only sells of 3 — the first consumes the whole
        // position, the second gets nothing.
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_002, -3, 30);
        let user_state = user_state_with(3, 60, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        assert_eq!(outcome.removed, vec![Uint64::new(2)]);
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_001, 1).unwrap().size,
            Quantity::new_int(-3)
        );
        assert!(load_ask(&ctx.storage, &pair(), 2_002, 2).is_none());
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::new_int(30));
        assert_eq!(outcome.user_state.open_order_count, 1);
    }

    /// A flat position leaves every reduce-only order on the wrong side ⇒ all are
    /// cancelled.
    #[test]
    fn cancel_all_when_flat() {
        let mut ctx = MockContext::new();

        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_002, -3, 30);
        // Flat (no position), but the orders + reserved still on record.
        let user_state = user_state_with(0, 60, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        assert_eq!(outcome.removed.len(), 2);
        assert!(load_ask(&ctx.storage, &pair(), 2_001, 1).is_none());
        assert!(load_ask(&ctx.storage, &pair(), 2_002, 2).is_none());
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::ZERO);
        assert_eq!(outcome.user_state.open_order_count, 0);
    }

    /// After a flip to short, the reduce-only *sells* are now on the wrong side
    /// and are cancelled, while a correctly-sided reduce-only buy is kept within
    /// the new (short) budget.
    #[test]
    fn cancel_wrong_side_after_flip() {
        let mut ctx = MockContext::new();

        // Two stale reduce-only sells (wrong side for a short) and one
        // reduce-only buy of 5 (closes the short, within the 5 budget).
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_002, -3, 30);
        save_ro_bid(&mut ctx.storage, &pair(), 3, 1_999, 5, 50);
        let user_state = user_state_with(-5, 110, 3);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        assert_eq!(outcome.removed.len(), 2);
        assert!(outcome.removed.contains(&Uint64::new(1)));
        assert!(outcome.removed.contains(&Uint64::new(2)));
        assert!(load_ask(&ctx.storage, &pair(), 2_001, 1).is_none());
        assert!(load_ask(&ctx.storage, &pair(), 2_002, 2).is_none());
        // The buy survives untouched.
        assert_eq!(
            load_bid(&ctx.storage, &pair(), 1_999, 3).unwrap().size,
            Quantity::new_int(5)
        );
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::new_int(50));
        assert_eq!(outcome.user_state.open_order_count, 1);
    }

    /// A re-size of one pair must not touch the user's reduce-only orders in
    /// another pair.
    #[test]
    fn scoped_to_one_pair() {
        let mut ctx = MockContext::new();

        // Oversized reduce-only sell in `pair()` (will shrink 10 → 5) and an
        // identical one in `other_pair()` that must be left alone.
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -10, 100);
        save_ro_ask(&mut ctx.storage, &other_pair(), 2, 3_001, -10, 100);
        let user_state = user_state_with(5, 200, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        // `pair()` order shrunk to 5, reserved halved.
        let shrunk = load_ask(&ctx.storage, &pair(), 2_001, 1).unwrap();
        assert_eq!(shrunk.size, Quantity::new_int(-5));
        assert_eq!(shrunk.reserved_margin, UsdValue::new_int(50));
        // `other_pair()` order byte-for-byte untouched.
        let untouched = load_ask(&ctx.storage, &other_pair(), 3_001, 2).unwrap();
        assert_eq!(untouched.size, Quantity::new_int(-10));
        assert_eq!(untouched.reserved_margin, UsdValue::new_int(100));
        // Aggregate reserved = 50 (pair) + 100 (other) = 150.
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::new_int(150));
        assert_eq!(
            outcome.user_state.reserved_margin,
            total_resting_reserved(&ctx.storage)
        );
    }

    /// Short-maker mirror of [`shrink_worst_first_clamps_the_sum`]: two
    /// reduce-only BUYS summing over the (short) position. Exercises the
    /// bid-side shrink path — a bid being shrunk (not merely kept or cancelled)
    /// — which the other tests never hit (they all use long makers / asks).
    #[test]
    fn shrink_bid_for_short_maker() {
        let mut ctx = MockContext::new();

        // Short 4; two reduce-only buys of 3 each (each ≤ 4, together 6 > 4).
        save_ro_bid(&mut ctx.storage, &pair(), 1, 1_999, 3, 30);
        save_ro_bid(&mut ctx.storage, &pair(), 2, 1_998, 3, 30);
        let user_state = user_state_with(-4, 60, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        // @1999 (best bid = highest real price) keeps 3; @1998 shrinks to 1.
        assert!(outcome.removed.is_empty());
        assert_eq!(
            load_bid(&ctx.storage, &pair(), 1_999, 1).unwrap().size,
            Quantity::new_int(3)
        );
        let shrunk = load_bid(&ctx.storage, &pair(), 1_998, 2).unwrap();
        assert_eq!(shrunk.size, Quantity::new_int(1));
        // Reserved released proportionally: 30 × 2/3 = 20, leaving 10.
        assert_eq!(shrunk.reserved_margin, UsdValue::new_int(10));
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::new_int(40));
        assert_eq!(
            outcome.user_state.reserved_margin,
            total_resting_reserved(&ctx.storage)
        );
        assert_eq!(outcome.user_state.open_order_count, 2);
    }

    /// Bid-side mirror of [`cancel_all_when_flat`]: a flat position cancels
    /// reduce-only *buys* too (the only flat/wrong-side test otherwise uses
    /// asks).
    #[test]
    fn cancel_wrong_side_bids_when_flat() {
        let mut ctx = MockContext::new();

        save_ro_bid(&mut ctx.storage, &pair(), 1, 1_999, 3, 30);
        save_ro_bid(&mut ctx.storage, &pair(), 2, 1_998, 3, 30);
        let user_state = user_state_with(0, 60, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        assert_eq!(outcome.removed.len(), 2);
        assert!(outcome.removed.contains(&Uint64::new(1)));
        assert!(outcome.removed.contains(&Uint64::new(2)));
        assert!(load_bid(&ctx.storage, &pair(), 1_999, 1).is_none());
        assert!(load_bid(&ctx.storage, &pair(), 1_998, 2).is_none());
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::ZERO);
        assert_eq!(outcome.user_state.open_order_count, 0);
    }

    /// Three reduce-only orders summing over the budget in a single pass:
    /// keep the best, shrink the middle, cancel the worst. The 2-order tests
    /// never exercise keep+shrink+cancel together.
    #[test]
    fn shrink_and_cancel_across_three_orders() {
        let mut ctx = MockContext::new();

        // Long 5; three reduce-only sells of 3 each (Σ9 > 5).
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_002, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 3, 2_003, -3, 30);
        let user_state = user_state_with(5, 90, 3);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        // @2001 keeps 3 (budget 5→2); @2002 shrinks to 2 (budget 2→0); @2003
        // gets nothing and is cancelled.
        assert_eq!(outcome.removed, vec![Uint64::new(3)]);
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_001, 1).unwrap().size,
            Quantity::new_int(-3)
        );
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_002, 2).unwrap().size,
            Quantity::new_int(-2)
        );
        assert!(load_ask(&ctx.storage, &pair(), 2_003, 3).is_none());
        // 90 − (10 released on the shrink) − (30 released on the cancel) = 50.
        assert_eq!(outcome.user_state.reserved_margin, UsdValue::new_int(50));
        assert_eq!(
            outcome.user_state.reserved_margin,
            total_resting_reserved(&ctx.storage)
        );
        assert_eq!(outcome.user_state.open_order_count, 2);
    }

    /// Two reduce-only orders at the SAME price: the budget is allocated by
    /// price-time priority, and ties break on ascending `order_id` (older
    /// first). The older order is kept, the newer one shrunk.
    #[test]
    fn tie_break_keeps_older_order_at_same_price() {
        let mut ctx = MockContext::new();

        // Long 4; two reduce-only sells of 3 each at the identical price 2_001.
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 30);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_001, -3, 30);
        let user_state = user_state_with(4, 60, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        // Order id 1 (older) keeps 3; order id 2 (newer) shrinks to 1.
        assert!(outcome.removed.is_empty());
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_001, 1).unwrap().size,
            Quantity::new_int(-3)
        );
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_001, 2).unwrap().size,
            Quantity::new_int(-1)
        );
        assert_eq!(outcome.user_state.open_order_count, 2);
    }

    /// Margin conservation holds even when the proportional release truncates.
    /// Shrinking a `-3` order to `-1` releases `10 × 2/3 = 6.666666` (Dec128_6
    /// truncates `6.6666…`), leaving `10 − 6.666666 = 3.333334`. Because the
    /// kept value is a single subtraction of the one truncated release (not a
    /// second division), `kept + released == reserved` exactly and no margin is
    /// stranded — the aggregate still equals the sum of resting reserved.
    #[test]
    fn shrink_margin_conserved_with_truncating_division() {
        let mut ctx = MockContext::new();

        // Long 4; two reduce-only sells of 3, each reserving an indivisible 10.
        save_ro_ask(&mut ctx.storage, &pair(), 1, 2_001, -3, 10);
        save_ro_ask(&mut ctx.storage, &pair(), 2, 2_002, -3, 10);
        let user_state = user_state_with(4, 20, 2);

        let outcome =
            compute_resize_reduce_only_outcome(&mut ctx.storage, USER, &pair(), &user_state, None)
                .unwrap();

        // @2001 keeps its 10; @2002 shrinks 3→1, keeping 10 − 6.666666.
        assert_eq!(
            load_ask(&ctx.storage, &pair(), 2_001, 1)
                .unwrap()
                .reserved_margin,
            UsdValue::new_int(10)
        );
        let shrunk = load_ask(&ctx.storage, &pair(), 2_002, 2).unwrap();
        assert_eq!(shrunk.size, Quantity::new_int(-1));
        assert_eq!(shrunk.reserved_margin, UsdValue::new_raw(3_333_334));
        // Aggregate == Σ resting (no stranded margin) and == 10 + 3.333334.
        assert_eq!(
            outcome.user_state.reserved_margin,
            UsdValue::new_raw(13_333_334)
        );
        assert_eq!(
            outcome.user_state.reserved_margin,
            total_resting_reserved(&ctx.storage)
        );
    }
}
