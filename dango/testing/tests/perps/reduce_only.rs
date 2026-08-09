//! Tests for the reduce-only order invariant: a user's resting reduce-only
//! orders may only move their position toward zero — never grow it, never flip
//! it. The invariant is:
//!
//! > For every (user, pair): the resting reduce-only orders are all on the
//! > position-closing side, and their absolute sizes sum to no more than the
//! > `|position|`.
//!
//! It is enforced by three cooperating layers:
//!
//! - **Placement** (`compute_submit_order_outcome` + the re-size in
//!   `_submit_order`): a new reduce-only order is clamped to the *remaining*
//!   budget after the user's other reduce-only orders; if the sum-clamp leaves
//!   it nothing, the transaction is rejected. A better-priced new order instead
//!   evicts the user's worse-priced ones.
//! - **Dynamic re-size** (`resize_reduce_only_orders`): after every position
//!   change — a fill (as taker or maker), liquidation, or ADL — the user's
//!   resting reduce-only orders are re-clamped to the new position, shrinking or
//!   cancelling the worst by price-time priority.
//! - **Match-time clamp** (`walk_book`): within a single sweep, before the
//!   dynamic re-size has run, each reduce-only maker fill is re-clamped against
//!   the maker's running position.
//!
//! Test convention: the reduce-only order under test is owned by `user2`. That
//! owner moves its own position either as a **taker** (it submits the
//! position-changing order itself — `submit_order.rs` re-sizes the sender) or as
//! a **maker** (a different user's taker fills one of user2's *other* orders —
//! the sender's maker loop re-sizes filled makers). The re-size fires in both
//! cases. A comment that says "user2 sells/buys …" means user2 is acting as the
//! taker, even though user2 is the maker of the reduce-only order being asserted.
//!
//! See `dango/exchange/perps/src/trade/resize_reduce_only.rs`, `submit_order.rs`, and
//! `dango/exchange/order-book/src/matching_engine.rs`.

use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::Uint128,
    dango_order_book::{
        ChildOrder, Dimensionless, OrderId, OrderKind, OrderRemoved, OrderResized, PairId,
        Quantity, QueryOrdersByUserResponseItem, ReasonForOrderRemoval, TimeInForce,
        TriggerDirection, UsdPrice,
    },
    dango_primitives::{
        Addr, Addressable, CheckedContractEvent, Coins, Duration, JsonDeExt, NonEmpty, QuerierExt,
        ResultExt, SearchEvent, btree_map, btree_set,
    },
    dango_testing::{TestAccount, TestOption, TestSuiteNaive, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, PairParam, PairState, UserState},
    },
    std::collections::BTreeMap,
};

/// $50,000 USDC (6 decimals) — generous so margin never clouds the
/// position-size assertions, while staying within each test account's balance.
const DEPOSIT: u128 = 50_000_000_000;

// --------------------------------- helpers -----------------------------------

/// Deposit `amount` (USDC base units) of margin for `account`.
async fn deposit(
    suite: &mut TestSuiteNaive,
    account: &mut TestAccount,
    contract: Addr,
    amount: u128,
) {
    suite
        .execute(
            account,
            contract,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(amount)).unwrap(),
        )
        .await
        .should_succeed();
}

/// Submit a post-only limit order, which rests on the book without matching.
/// Positive `size` is a bid (buy), negative an ask (sell).
async fn rest_limit(
    suite: &mut TestSuiteNaive,
    account: &mut TestAccount,
    contract: Addr,
    pair: &PairId,
    size: i128,
    price: i128,
    reduce_only: bool,
) {
    suite
        .execute(
            account,
            contract,
            &post_only(pair, size, price, reduce_only),
            Coins::new(),
        )
        .await
        .should_succeed();
}

/// Build a post-only limit order request (positive `size` is a bid).
fn post_only(pair: &PairId, size: i128, price: i128, reduce_only: bool) -> perps::ExecuteMsg {
    perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
        pair_id: pair.clone(),
        size: Quantity::new_int(size),
        kind: OrderKind::Limit {
            limit_price: UsdPrice::new_int(price),
            time_in_force: TimeInForce::PostOnly,
            client_order_id: None,
        },
        reduce_only,
        tp: None,
        sl: None,
    }))
}

/// Build a market order request with a generous 50% slippage tolerance.
fn market(pair: &PairId, size: i128, reduce_only: bool) -> perps::ExecuteMsg {
    perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
        pair_id: pair.clone(),
        size: Quantity::new_int(size),
        kind: OrderKind::Market {
            max_slippage: Dimensionless::new_percent(50),
        },
        reduce_only,
        tp: None,
        sl: None,
    }))
}

/// Submit a market order that is expected to (at least partially) fill.
async fn market_fill(
    suite: &mut TestSuiteNaive,
    account: &mut TestAccount,
    contract: Addr,
    pair: &PairId,
    size: i128,
    reduce_only: bool,
) {
    suite
        .execute(
            account,
            contract,
            &market(pair, size, reduce_only),
            Coins::new(),
        )
        .await
        .should_succeed();
}

fn user_state(suite: &TestSuiteNaive, contract: Addr, user: Addr) -> UserState {
    suite
        .query_wasm_smart(contract, perps::QueryUserStateRequest { user })
        .should_succeed()
        .unwrap()
}

/// The user's signed position size in `pair`, or `None` if the user has no
/// position there (a position closed to exactly zero is removed from the map).
fn position(suite: &TestSuiteNaive, contract: Addr, user: Addr, pair: &PairId) -> Option<Quantity> {
    user_state(suite, contract, user)
        .positions
        .get(pair)
        .map(|p| p.size)
}

fn open_orders(
    suite: &TestSuiteNaive,
    contract: Addr,
    user: Addr,
) -> BTreeMap<OrderId, QueryOrdersByUserResponseItem> {
    suite
        .query_wasm_smart(contract, perps::QueryOrdersByUserRequest { user })
        .should_succeed()
}

/// The user's single resting order at `price`, if any.
fn order_at(
    suite: &TestSuiteNaive,
    contract: Addr,
    user: Addr,
    price: i128,
) -> Option<QueryOrdersByUserResponseItem> {
    open_orders(suite, contract, user)
        .into_values()
        .find(|o| o.limit_price == UsdPrice::new_int(price))
}

/// The pair's state, carrying `long_oi` / `short_oi`. Open interest is a strong
/// cross-check on the clamp: a fill that wrongly opened a position would inflate
/// OI, and after every fill the invariant `long_oi == short_oi` must hold.
fn pair_state(suite: &TestSuiteNaive, contract: Addr, pair: &PairId) -> PairState {
    suite
        .query_wasm_smart(
            contract,
            perps::QueryPairStateRequest {
                pair_id: pair.clone(),
            },
        )
        .should_succeed()
        .unwrap()
}

// ----------------------------- placement clamp -------------------------------

/// Placement rejects a reduce-only order whose size, summed with the user's
/// existing reduce-only orders, would exceed the position. This is the
/// defense-in-depth that kills the "many copies of a 1-unit reduce-only order"
/// attack at the source: the second copy can never rest.
///
/// A maker opens a 1-unit long and rests a reduce-only sell of 1 (the whole
/// position). A second reduce-only sell of 1 is rejected — there is no position
/// budget left for it. The single resting order then closes the long normally.
#[tokio::test]
async fn reduce_only_placement_rejects_when_sum_exceeds_position() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    // user3 rests an ask of 1 so the maker can open a long.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -1,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 1, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(1))
    );

    // First reduce-only sell of 1 rests — it exactly fills the position budget.
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -1,
        2_001,
        true,
    )
    .await;

    // Second reduce-only sell of 1 is rejected: the sum (2) would exceed the
    // position (1). It is a post-only order — i.e. one that *would* rest — so
    // the sum-clamp actually fires (an IOC/market order would just not fill).
    suite
        .execute(
            &mut accounts.user2,
            perps,
            &post_only(&pair, -1, 2_002, true),
            Coins::new(),
        )
        .await
        .should_fail_with_error("reduce-only order would exceed position size");

    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1,
        "only the first reduce-only order rests; the second was rejected"
    );

    // The one valid order closes the long normally.
    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 1, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(1))
    );
    assert!(open_orders(&suite, perps, accounts.user2.address()).is_empty());

    let ps = pair_state(&suite, perps, &pair);
    assert_eq!(ps.long_oi, Quantity::new_int(1));
    assert_eq!(ps.short_oi, Quantity::new_int(1));
}

/// Short-maker mirror of [`reduce_only_placement_rejects_when_sum_exceeds_position`]:
/// a 1-unit short with reduce-only *buys*. The second buy is rejected.
#[tokio::test]
async fn reduce_only_placement_rejects_short_maker() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    // user3 rests a bid of 1 so the maker can open a short.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        1,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -1, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(-1))
    );

    // First reduce-only buy of 1 rests; the second is rejected.
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        1,
        1_999,
        true,
    )
    .await;
    suite
        .execute(
            &mut accounts.user2,
            perps,
            &post_only(&pair, 1, 1_998, true),
            Coins::new(),
        )
        .await
        .should_fail_with_error("reduce-only order would exceed position size");

    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );

    market_fill(&mut suite, &mut accounts.user1, perps, &pair, -1, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(-1))
    );
}

/// A better-priced new reduce-only order evicts the user's own worse-priced
/// one: re-quoting at a price more likely to fill. The new order takes the whole
/// budget; the stale one is cancelled.
///
/// Maker is long 5 with a reduce-only sell of 5 resting at 2005. A new
/// reduce-only sell of 5 at 2002 (better — a lower ask fills first) takes the
/// whole 5-unit budget, so the 2005 order is cancelled.
#[tokio::test]
async fn reduce_only_placement_better_price_replaces_worse() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -5,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 5, false).await;

    // Worse-priced reduce-only sell first, then a better-priced one.
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_005,
        true,
    )
    .await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_002,
        true,
    )
    .await;

    // Only the better-priced order survives; the worse one was evicted.
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_002)
            .unwrap()
            .size,
        Quantity::new_int(-5)
    );
    assert!(order_at(&suite, perps, accounts.user2.address(), 2_005).is_none());
}

/// A new reduce-only order is shrunk to the budget left by the user's existing
/// reduce-only orders, rather than rejected, when that budget is positive.
///
/// Maker is long 10 with a reduce-only sell of 6 at 2001 (better). A new
/// reduce-only sell of 10 at 2002 (worse) is shrunk to 4 — the budget left after
/// the 6 — and both rest.
#[tokio::test]
async fn reduce_only_placement_partial_budget_shrinks_new() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;

    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -6,
        2_001,
        true,
    )
    .await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_002,
        true,
    )
    .await;

    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        2
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_001)
            .unwrap()
            .size,
        Quantity::new_int(-6)
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_002)
            .unwrap()
            .size,
        Quantity::new_int(-4),
        "the worse order is shrunk to the remaining budget (10 - 6)"
    );
}

// --------------------------- dynamic re-sizing -------------------------------

/// A reduce-only order is re-sized down when its owner's position shrinks via a
/// separate (taker) fill — *before* any taker reaches it. With the order then
/// already right-sized, the match-time clamp has nothing left to do: a taker
/// buying exactly the resized amount fills it in full, with no mid-sweep skip.
/// This is the layering thesis — eager re-sizing makes the match-time clamp
/// redundant outside a single sweep.
#[tokio::test]
async fn reduce_only_resize_on_taker_fill_then_clamp_is_noop() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // Maker (user2) opens a 10-unit long and rests a reduce-only sell of 10.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_002,
        true,
    )
    .await;

    let reserved_before = user_state(&suite, perps, accounts.user2.address()).reserved_margin;

    // The owner (user2), acting as the taker, shrinks its own long to 4 by
    // selling 6 into user4's bid.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        6,
        2_001,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -6, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(4))
    );

    // The reduce-only order was re-sized 10 -> 4 the instant the position
    // shrank, with reserved margin released proportionally.
    let resting = order_at(&suite, perps, accounts.user2.address(), 2_002).unwrap();
    assert_eq!(
        resting.size,
        Quantity::new_int(-4),
        "re-sized to the new position"
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );
    assert!(
        user_state(&suite, perps, accounts.user2.address()).reserved_margin < reserved_before,
        "shrinking the order released part of its reserved margin"
    );

    // A taker buys exactly 4: the order is already right-sized, so it fills in
    // full and is removed — the match-time clamp had nothing to clamp.
    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 4, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(4))
    );
    assert!(open_orders(&suite, perps, accounts.user2.address()).is_empty());

    let ps = pair_state(&suite, perps, &pair);
    assert_eq!(ps.long_oi, ps.short_oi, "OI must net");
    assert_eq!(ps.long_oi, Quantity::new_int(10));
}

/// A reduce-only order is re-sized when its owner's position shrinks because one
/// of their *other*, non-reduce-only orders is filled (the owner is the maker,
/// not the taker). Also asserts the `OrderResized` event.
///
/// Maker is long 10, resting a non-reduce-only sell of 6 (at 2001) and a
/// reduce-only sell of 10 (at 2003). A taker buys 6, filling only the non-RO
/// order; the maker drops to long 4, and its reduce-only order is re-sized to 4.
#[tokio::test]
async fn reduce_only_resize_on_maker_fill_shrinks_order() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;

    // Non-reduce-only sell of 6 (senior) and reduce-only sell of 10.
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -6,
        2_001,
        false,
    )
    .await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_003,
        true,
    )
    .await;

    // Taker buys 6 — fills only the non-RO order, dropping the maker to long 4.
    let events = suite
        .execute(
            &mut accounts.user1,
            perps,
            &market(&pair, 6, false),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(4))
    );

    // The non-RO order filled and was removed; the reduce-only order was re-sized
    // 10 -> 4.
    assert!(order_at(&suite, perps, accounts.user2.address(), 2_001).is_none());
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_003)
            .unwrap()
            .size,
        Quantity::new_int(-4)
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );

    // The re-size emitted an `OrderResized` event carrying old and new size.
    let resized = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_resized")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderResized>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(resized.len(), 1, "exactly one order was shrunk");
    assert_eq!(resized[0].user, accounts.user2.address());
    assert_eq!(resized[0].old_size, Quantity::new_int(-10));
    assert_eq!(resized[0].new_size, Quantity::new_int(-4));
    // The remaining event fields identify the order: it is the reduce-only sell
    // still resting at 2_003, placed without a client_order_id.
    assert_eq!(resized[0].pair_id, pair);
    assert_eq!(resized[0].client_order_id, None);
    let resized_order_id = open_orders(&suite, perps, accounts.user2.address())
        .into_iter()
        .find(|(_, o)| o.limit_price == UsdPrice::new_int(2_003))
        .map(|(id, _)| id)
        .expect("reduce-only order still resting at 2_003");
    assert_eq!(resized[0].order_id, resized_order_id);
}

/// When the owner's position flips, every reduce-only order is on the wrong side
/// and is cancelled (not merely skipped). Asserts the `OrderRemoved` event with
/// the `ReduceOnlyResized` reason, and that a taker then finds no liquidity.
///
/// Maker is long 10 with a reduce-only sell of 10 resting, then flips to a 5-unit
/// short. The reduce-only sell would now grow the short, so it is cancelled.
#[tokio::test]
async fn reduce_only_resize_cancels_on_flip() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_002,
        true,
    )
    .await;

    // The owner (user2), acting as the taker, flips its own position to a
    // 5-unit short by selling 15 into user4's bid.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        15,
        2_001,
        false,
    )
    .await;
    let events = suite
        .execute(
            &mut accounts.user2,
            perps,
            &market(&pair, -15, false),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(-5))
    );

    // The reduce-only sell was cancelled — not left resting.
    assert!(
        open_orders(&suite, perps, accounts.user2.address()).is_empty(),
        "the wrong-side reduce-only order is cancelled on the flip"
    );

    let removed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_removed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderRemoved>().unwrap())
        .filter(|e| e.reason == ReasonForOrderRemoval::ReduceOnlyResized)
        .collect::<Vec<_>>();
    assert_eq!(
        removed.len(),
        1,
        "one reduce-only order cancelled by re-sizing"
    );
    assert_eq!(removed[0].user, accounts.user2.address());

    // A taker now finds no resting ask to hit.
    suite
        .execute(
            &mut accounts.user1,
            perps,
            &market(&pair, 10, false),
            Coins::new(),
        )
        .await
        .should_fail_with_error("no liquidity at acceptable price");
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(-5)),
        "the short is untouched"
    );
}

/// Within a single sweep, an earlier non-reduce-only order flips the maker past
/// zero; the reduce-only order reached later is skipped by the match-time clamp,
/// then cancelled by the post-sweep re-size.
///
/// Maker is long 5, resting a non-reduce-only sell of 10 (senior) and a
/// reduce-only sell of 5. A taker buys 20: the non-RO order fills 10, flipping
/// the maker to a 5-unit short; the reduce-only order is inert and is cancelled.
#[tokio::test]
async fn reduce_only_resize_after_earlier_order_flips_maker() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -5,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 5, false).await;

    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_001,
        false,
    )
    .await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_002,
        true,
    )
    .await;

    let events = suite
        .execute(
            &mut accounts.user1,
            perps,
            &market(&pair, 20, false),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    // A flipped the maker 5 -> -5; B (reduce-only) was skipped then cancelled.
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(-5))
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(10))
    );
    assert!(
        open_orders(&suite, perps, accounts.user2.address()).is_empty(),
        "the inert reduce-only order is cancelled by re-sizing"
    );

    // The cancel here is caused by a MAKER fill (user2's senior order flips the
    // position mid-sweep), not a taker action — assert the post-sweep re-size
    // emitted `OrderRemoved{ReduceOnlyResized}` for user2's inert order.
    let removed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_removed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderRemoved>().unwrap())
        .filter(|e| e.reason == ReasonForOrderRemoval::ReduceOnlyResized)
        .collect::<Vec<_>>();
    assert_eq!(
        removed.len(),
        1,
        "one reduce-only order cancelled by re-sizing"
    );
    assert_eq!(removed[0].user, accounts.user2.address());

    let ps = pair_state(&suite, perps, &pair);
    assert_eq!(ps.long_oi, Quantity::new_int(10));
    assert_eq!(ps.short_oi, Quantity::new_int(10));
}

/// Several of the SAME maker's orders are matched in one sweep, where an earlier
/// non-reduce-only order reduces the position before a later reduce-only order is
/// reached. The reduce-only order's match-time clamp reflects the earlier fill
/// (filling only what remains), and the post-sweep re-size cancels its now-inert
/// remainder.
///
/// Maker is long 10, resting a non-reduce-only sell of 6 (senior) and a
/// reduce-only sell of 10. A taker buys 16: the non-RO closes 6 (-> 4), the
/// reduce-only clamps to 4 and closes it (-> 0), and its -6 remainder is then
/// cancelled.
#[tokio::test]
async fn reduce_only_clamp_reflects_earlier_fill_in_same_sweep() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;

    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -6,
        2_001,
        false,
    )
    .await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_002,
        true,
    )
    .await;

    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 16, false).await;

    // Maker ends flat; the reduce-only order clamped to the reduced position
    // (filling 4) and its -6 remainder was cancelled — not left resting.
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(10))
    );
    assert!(
        open_orders(&suite, perps, accounts.user2.address()).is_empty(),
        "the inert remainder is cancelled by re-sizing"
    );

    let ps = pair_state(&suite, perps, &pair);
    assert_eq!(ps.long_oi, Quantity::new_int(10));
    assert_eq!(ps.short_oi, Quantity::new_int(10));
}

/// One taker sweeps reduce-only orders from two different makers; each must clamp
/// to its OWN position. One maker's order was already re-sized down when that
/// maker's position shrank, so the sweep sees the correct, smaller order.
///
/// user2 and user3 each open a 5-unit long and rest a reduce-only sell of 5.
/// user3 then shrinks to 1 — re-sizing its order to 1. A taker buys 10: user2
/// closes its full 5, user3 closes its 1; both end flat.
#[tokio::test]
async fn reduce_only_resize_per_maker_across_two_makers() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // user4 sells 10; user2 and user3 each buy 5.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 5, false).await;
    market_fill(&mut suite, &mut accounts.user3, perps, &pair, 5, false).await;

    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_001,
        true,
    )
    .await;
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -5,
        2_001,
        true,
    )
    .await;

    // user3 shrinks its long to 1 by selling 4 into user4's bid — its reduce-only
    // order is re-sized 5 -> 1 at that moment.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        4,
        1_999,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user3, perps, &pair, -4, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user3.address(), &pair),
        Some(Quantity::new_int(1))
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user3.address(), 2_001)
            .unwrap()
            .size,
        Quantity::new_int(-1),
        "user3's reduce-only order was re-sized to its new position"
    );

    // Taker buys 10: user2's 5 (senior) then user3's 1.
    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 10, false).await;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None
    );
    assert_eq!(
        position(&suite, perps, accounts.user3.address(), &pair),
        None
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(6))
    );
    assert!(open_orders(&suite, perps, accounts.user2.address()).is_empty());
    assert!(open_orders(&suite, perps, accounts.user3.address()).is_empty());

    let ps = pair_state(&suite, perps, &pair);
    assert_eq!(ps.long_oi, Quantity::new_int(6));
    assert_eq!(ps.short_oi, Quantity::new_int(6));
}

/// Re-sizing only ever shrinks/cancels — it never grows a reduce-only order. When
/// the position grows, the order is left exactly as it was (no resurrection).
///
/// Maker is long 5 with a reduce-only sell of 5; the maker buys 5 more (long 10).
/// The order stays at 5.
#[tokio::test]
async fn reduce_only_position_growth_leaves_order() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -5,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 5, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_002,
        true,
    )
    .await;

    // The owner (user2), acting as the taker, grows its own long to 10 by
    // buying 5 from user4.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        -5,
        2_001,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 5, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(10))
    );

    // The reduce-only order is untouched.
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_002)
            .unwrap()
            .size,
        Quantity::new_int(-5),
        "growth never grows or resurrects the order"
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );
}

// ------------------------------- regression ----------------------------------

/// Regression: when the position fully covers the reduce-only order, the order
/// fills in full and is removed — the clamp must not under-fill it.
#[tokio::test]
async fn reduce_only_fills_fully_when_position_sufficient() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;

    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_002,
        true,
    )
    .await;

    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 5, false).await;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(5))
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(5))
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        0
    );
}

/// Regression: the clamp applies ONLY to reduce-only orders. A regular resting
/// order may still flip the maker's position — normal trading behavior.
#[tokio::test]
async fn non_reduce_only_order_still_flips_position() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -1,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 1, false).await;

    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_002,
        false,
    )
    .await;

    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 5, false).await;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(-4)),
        "a non-reduce-only order may flip the position"
    );
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(5))
    );
}

// ----------------------------- liquidation / ADL -----------------------------

/// A reduce-only order is re-sized when its owner's position is forcibly reduced
/// as an ADL counter-party during someone else's liquidation. This is the
/// liquidation-path trigger for dynamic re-sizing.
///
/// Carol (user3) is long 5 with a reduce-only sell of 5 resting far out of the
/// money. user1 holds a 3-unit short that goes underwater and is liquidated;
/// with no in-range book liquidity, all 3 are ADL'd against Carol's long, taking
/// it to 2. Carol's reduce-only order is re-sized 5 -> 2.
#[tokio::test]
async fn reduce_only_resize_on_adl_counterparty() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    // Widen the price band so Carol's far-OTM reduce-only ask can be placed and,
    // crucially, so the liquidation's market buy skips it (it sits above the
    // oracle-clamped target price) and falls through to ADL.
    suite
        .execute(
            &mut accounts.owner,
            perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: default_param(),
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        max_limit_price_deviation: Dimensionless::new_permille(999),
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Carol (user3) and user4 are well funded; user1 (the short) is funded just
    // enough to open and be liquidatable when the oracle rises.
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user1, perps, 700_000_000).await;

    // Carol rests a bid of 5; user1 sells 3 and user4 sells 2 into it, leaving
    // Carol long 5, user1 short 3, user4 short 2 (OI balanced).
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        5,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user1, perps, &pair, -3, false).await;
    market_fill(&mut suite, &mut accounts.user4, perps, &pair, -2, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user3.address(), &pair),
        Some(Quantity::new_int(5))
    );

    // Carol rests a reduce-only sell of 5 far out of the money.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -5,
        3_900,
        true,
    )
    .await;

    // Oracle rises: user1's short is now underwater.
    register_oracle_prices(&mut suite, &mut accounts, 2_300).await;

    // Liquidate user1. No in-range asks (Carol's is at 3,900, above the oracle
    // target), so all 3 are ADL'd against Carol's long (5 -> 2).
    suite
        .execute(
            &mut accounts.owner,
            perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Liquidate {
                user: accounts.user1.address(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    assert_eq!(
        position(&suite, perps, accounts.user3.address(), &pair),
        Some(Quantity::new_int(2)),
        "Carol's long was reduced to 2 by ADL"
    );

    // Carol's reduce-only order was re-sized to her new position.
    assert_eq!(
        order_at(&suite, perps, accounts.user3.address(), 3_900)
            .unwrap()
            .size,
        Quantity::new_int(-2),
        "the reduce-only order tracks the ADL'd-down position"
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user3.address()).open_order_count,
        1
    );
}

// ------------------------- short-maker dynamic re-size -----------------------

/// Short-maker mirror of [`reduce_only_resize_on_maker_fill_shrinks_order`]: a
/// SHORT maker's reduce-only BUY is shrunk when its position is reduced by one
/// of its own non-reduce-only orders filling. Closes the bid-side gap left by
/// the rewrite (every other dynamic test uses a long maker / reduce-only sells).
#[tokio::test]
async fn reduce_only_resize_short_maker_buy_shrinks() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    // user2 opens a 10-unit short against user3's bid.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -10, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(-10))
    );

    // Non-reduce-only BUY of 6 (senior, best bid 1999) and reduce-only BUY of 10.
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        6,
        1_999,
        false,
    )
    .await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        10,
        1_997,
        true,
    )
    .await;

    // Taker sells 6 — fills only the non-RO buy, lifting the short to -4.
    let events = suite
        .execute(
            &mut accounts.user1,
            perps,
            &market(&pair, -6, false),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(-4))
    );

    // The non-RO buy filled and was removed; the reduce-only buy resized 10 -> 4.
    assert!(order_at(&suite, perps, accounts.user2.address(), 1_999).is_none());
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 1_997)
            .unwrap()
            .size,
        Quantity::new_int(4),
        "short maker's reduce-only buy tracks the reduced short"
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );

    let resized = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_resized")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderResized>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(resized.len(), 1);
    assert_eq!(resized[0].old_size, Quantity::new_int(10));
    assert_eq!(resized[0].new_size, Quantity::new_int(4));
}

/// Short-maker mirror of [`reduce_only_resize_cancels_on_flip`]: when a short
/// flips to long, the reduce-only BUY is on the wrong side (buys grow a long)
/// and is cancelled.
#[tokio::test]
async fn reduce_only_resize_short_maker_cancels_on_flip() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // user2 opens a 10-unit short and rests a reduce-only buy of 10.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -10, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        10,
        1_998,
        true,
    )
    .await;

    // The owner (user2), acting as the taker, flips its short to a 5-unit long by
    // buying 15 from user4's ask.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        -15,
        1_999,
        false,
    )
    .await;
    let events = suite
        .execute(
            &mut accounts.user2,
            perps,
            &market(&pair, 15, false),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(5))
    );
    assert!(
        open_orders(&suite, perps, accounts.user2.address()).is_empty(),
        "the wrong-side reduce-only buy is cancelled on the flip"
    );

    let removed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_removed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderRemoved>().unwrap())
        .filter(|e| e.reason == ReasonForOrderRemoval::ReduceOnlyResized)
        .collect::<Vec<_>>();
    assert_eq!(
        removed.len(),
        1,
        "one reduce-only order cancelled by re-sizing"
    );
    assert_eq!(removed[0].user, accounts.user2.address());
}

// ------------------------------ batch + rollback -----------------------------

/// `batch_update_orders` runs the same re-size hook as a plain submit: an action
/// that reduces the sender's position inside a batch shrinks its resting
/// reduce-only order.
#[tokio::test]
async fn reduce_only_resize_within_batch() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // user2 opens a 10-unit long and rests a reduce-only sell of 10.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_002,
        true,
    )
    .await;

    // user4 rests a bid so the batch's sell can fill on the book.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        6,
        1_999,
        false,
    )
    .await;

    // A batch whose action reduces user2's long to 4 must trigger the re-size
    // hook reached via BatchUpdateOrders -> _submit_order.
    suite
        .execute(
            &mut accounts.user2,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![perps::SubmitOrCancelOrderRequest::Submit(
                    perps::SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(-6),
                        kind: OrderKind::Market {
                            max_slippage: Dimensionless::new_percent(50),
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    },
                )])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(4))
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_002)
            .unwrap()
            .size,
        Quantity::new_int(-4),
        "reduce-only sell re-sized inside the batch"
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );
}

/// A reduce-only placement rejection rolls back the *whole* batch, including an
/// earlier action's fill. Single-submit can't reach this (a filling reduce-only
/// order is always best-priced, so it is kept, never rejected); the batch makes
/// the fill and the rejected post-only separate actions.
#[tokio::test]
async fn reduce_only_batch_rejection_rolls_back_fill() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // user2 long 10 with a reduce-only sell of 10 at the better price 2001.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_001,
        true,
    )
    .await;

    // user4 bid for the batch's first action to fill against.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        4,
        1_999,
        false,
    )
    .await;

    // Batch: action 1 sells 4 (fills user4, long 10 -> 6, re-sizes the 2001 order
    // to -6); action 2 is a post-only reduce-only sell at the WORSE price 2005,
    // which the now-exhausted budget (6, fully held by the 2001 order) zeroes ->
    // reject. The reject must roll back action 1's fill too.
    suite
        .execute(
            &mut accounts.user2,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    perps::SubmitOrCancelOrderRequest::Submit(perps::SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(-4),
                        kind: OrderKind::Market {
                            max_slippage: Dimensionless::new_percent(50),
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    }),
                    perps::SubmitOrCancelOrderRequest::Submit(perps::SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(-10),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(2_005),
                            time_in_force: TimeInForce::PostOnly,
                            client_order_id: None,
                        },
                        reduce_only: true,
                        tp: None,
                        sl: None,
                    }),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_fail_with_error("reduce-only order would exceed position size");

    // Whole batch rolled back: position still 10, the RO sell still -10 at 2001,
    // and user4's bid untouched (the fill was undone).
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(10))
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_001)
            .unwrap()
            .size,
        Quantity::new_int(-10)
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user4.address(), 1_999)
            .unwrap()
            .size,
        Quantity::new_int(4),
        "user4's bid fill was rolled back"
    );
}

/// A single GTC reduce-only order that partially fills is necessarily the
/// best-priced reduce-only order, so the sum-clamp keeps it and EVICTS the
/// user's older worse-priced one — it is never itself rejected, and the fill
/// stands. (Complements the post-only eviction test, which never fills.)
#[tokio::test]
async fn reduce_only_partial_fill_evicts_worse_order() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    // user2 long 10 with a reduce-only sell of 10 at the WORSE price 2003.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_003,
        true,
    )
    .await;

    // user3 rests a bid of 3 at 2000 for the new order to partially fill against.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        3,
        2_000,
        false,
    )
    .await;

    // user2 submits a GTC reduce-only sell of 10 at the BETTER price 1999: it
    // crosses the bid (fills 3, long 10 -> 7) and rests the remaining 7.
    suite
        .execute(
            &mut accounts.user2,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_999),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: None,
                },
                reduce_only: true,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        Some(Quantity::new_int(7)),
        "the partial fill stands"
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 1_999)
            .unwrap()
            .size,
        Quantity::new_int(-7),
        "the new order rests at its remainder"
    );
    assert!(
        order_at(&suite, perps, accounts.user2.address(), 2_003).is_none(),
        "the older worse-priced order was evicted"
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user2.address()).open_order_count,
        1
    );
}

// -------------------------- depth / zombie guards ----------------------------

/// After a reduce-only order is shrunk, the book really holds only the shrunk
/// size at that price. Asserts the liquidity-depth map directly — it reports 4,
/// not the pre-shrink 10 — and that a taker can fill only the smaller amount.
#[tokio::test]
async fn reduce_only_depth_correct_after_shrink() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    // Configure a $1 depth bucket so an order's price is its own bucket key.
    suite
        .execute(
            &mut accounts.owner,
            perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: default_param(),
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        bucket_sizes: btree_set! { UsdPrice::new_int(1) },
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // Absolute ask depth at `price` (the $1 bucket key is the price itself), or
    // `None` if the book holds nothing there.
    let ask_depth_at = |suite: &TestSuiteNaive, price: i128| -> Option<Quantity> {
        suite
            .query_wasm_smart(
                perps,
                perps::QueryLiquidityDepthRequest {
                    pair_id: pair.clone(),
                    bucket_size: UsdPrice::new_int(1),
                    limit: None,
                },
            )
            .should_succeed()
            .asks
            .get(&UsdPrice::new_int(price))
            .map(|d| d.size)
    };

    // user2 long 10, reduce-only sell of 10 at 2002.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -10,
        2_002,
        true,
    )
    .await;

    // The depth map reports the full 10 at the 2002 ask bucket.
    assert_eq!(ask_depth_at(&suite, 2_002), Some(Quantity::new_int(10)));

    // user2 sells 6 (as taker) into user4's bid -> long 4, RO sell shrinks to -4.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        6,
        2_001,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -6, false).await;
    assert_eq!(
        order_at(&suite, perps, accounts.user2.address(), 2_002)
            .unwrap()
            .size,
        Quantity::new_int(-4)
    );

    // The depth map now reports only 4 at 2002 — the shrink decremented it.
    assert_eq!(
        ask_depth_at(&suite, 2_002),
        Some(Quantity::new_int(4)),
        "liquidity depth tracks the shrunk size"
    );

    // And behaviourally: a taker buying 10 fills only the shrunk 4; the remaining
    // 6 finds no liquidity (discarded).
    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 10, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(4)),
        "only the shrunk 4 was available"
    );
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None,
        "user2 closed flat"
    );
    assert!(open_orders(&suite, perps, accounts.user2.address()).is_empty());

    // The order fully filled, so the book holds nothing at 2002 now.
    assert_eq!(
        ask_depth_at(&suite, 2_002),
        None,
        "depth cleared after the fill"
    );

    let ps = pair_state(&suite, perps, &pair);
    assert_eq!(ps.long_oi, ps.short_oi, "OI must net");
}

/// Regression guard: after a position-changing tx leaves the owner flat, no
/// stale reduce-only order is left behind (the inert-cleanup must not depend on
/// the reduce-only order itself having filled).
#[tokio::test]
async fn reduce_only_no_zombie_after_position_change() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // user2 long 5 with a reduce-only sell parked high (won't fill).
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -5,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 5, false).await;
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        -5,
        2_005,
        true,
    )
    .await;

    // user2 closes its long to flat via a SEPARATE non-RO sell (not the RO order
    // filling) -> the now-stale reduce-only sell must be cancelled.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        5,
        1_999,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -5, false).await;

    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None,
        "user2 is flat"
    );
    assert!(
        open_orders(&suite, perps, accounts.user2.address()).is_empty(),
        "no zombie reduce-only order remains after closing to flat"
    );
}

// ----------------------- conditional-order trigger path ----------------------

/// A conditional (TP) trigger that partially closes a position re-sizes the
/// owner's *other* resting reduce-only order. Covers the
/// `process_conditional_orders` hook, which no other test exercises.
#[tokio::test]
async fn reduce_only_resize_on_conditional_trigger() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user1, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;

    // user1 opens a 10-unit long.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user1, perps, &pair, 10, false).await;

    // user1 rests a reduce-only sell of 10 above the TP maker bid (won't fill).
    rest_limit(
        &mut suite,
        &mut accounts.user1,
        perps,
        &pair,
        -10,
        2_150,
        true,
    )
    .await;

    // user1 attaches a partial take-profit: sell 6 when oracle >= 2100.
    suite
        .execute(
            &mut accounts.user1,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-6)),
                trigger_price: UsdPrice::new_int(2_100),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(1),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // user2 rests a bid of 6 at 2100 for the triggered market sell to fill.
    rest_limit(
        &mut suite,
        &mut accounts.user2,
        perps,
        &pair,
        6,
        2_100,
        false,
    )
    .await;

    // Oracle rises to the trigger and a block fires the cron. (`increase_time`
    // discards the outcome, so inline its body to read the cron events.)
    register_oracle_prices(&mut suite, &mut accounts, 2_100).await;
    let old_block_time = suite.block_time;
    suite.block_time = Duration::from_minutes(2);
    let outcome = suite.make_empty_block().await;
    suite.block_time = old_block_time;

    // TP closed 6 of user1's long; the reduce-only sell re-sized 10 -> 4.
    assert_eq!(
        position(&suite, perps, accounts.user1.address(), &pair),
        Some(Quantity::new_int(4))
    );
    assert_eq!(
        order_at(&suite, perps, accounts.user1.address(), 2_150)
            .unwrap()
            .size,
        Quantity::new_int(-4),
        "reduce-only sell tracks the TP-reduced position"
    );

    // The trigger and the re-size both surfaced in the cron block. (`search_event`
    // consumes the outcome, so clone it for the first of the two searches.)
    let triggered = outcome
        .block_outcome
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "conditional_order_triggered")
        .take()
        .all();
    assert_eq!(triggered.len(), 1, "the TP fired");
    let resized = outcome
        .block_outcome
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_resized")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderResized>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(resized.len(), 1);
    assert_eq!(resized[0].new_size, Quantity::new_int(-4));
}

// --------------------------- liquidation book fill ---------------------------

/// A maker who provides BOOK liquidity for a liquidation (not ADL) has its own
/// position reduced by that fill, re-sizing its resting reduce-only order. The
/// existing liquidation test only covers the ADL counter-party path.
#[tokio::test]
async fn reduce_only_resize_on_liquidation_book_maker() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    // Widen the band so Carol's far-OTM reduce-only ask survives the oracle move.
    suite
        .execute(
            &mut accounts.owner,
            perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: default_param(),
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        max_limit_price_deviation: Dimensionless::new_permille(999),
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user1, perps, 700_000_000).await;

    // Carol (user3) long 5; user1 short 3, user4 short 2.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        5,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user1, perps, &pair, -3, false).await;
    market_fill(&mut suite, &mut accounts.user4, perps, &pair, -2, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user3.address(), &pair),
        Some(Quantity::new_int(5))
    );

    // Carol rests a reduce-only sell of 5 far OTM (won't fill).
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -5,
        3_900,
        true,
    )
    .await;

    // Oracle rises: user1's short is now underwater.
    register_oracle_prices(&mut suite, &mut accounts, 2_300).await;

    // Carol rests an IN-RANGE regular ask of 3 (below the liquidation buy target),
    // so user1's liquidation fills on the BOOK rather than ADL.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -3,
        2_250,
        false,
    )
    .await;

    // Liquidate user1: it buys 3, filling Carol's 2250 ask -> Carol long 5 -> 2.
    suite
        .execute(
            &mut accounts.owner,
            perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Liquidate {
                user: accounts.user1.address(),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    assert_eq!(
        position(&suite, perps, accounts.user3.address(), &pair),
        Some(Quantity::new_int(2)),
        "Carol's long was reduced to 2 by the book fill"
    );
    // The book fill consumed Carol's 2250 ask (proves book path, not ADL)...
    assert!(
        order_at(&suite, perps, accounts.user3.address(), 2_250).is_none(),
        "the in-range ask was filled by the liquidation (book path)"
    );
    // ...and her reduce-only order tracked the reduced position.
    assert_eq!(
        order_at(&suite, perps, accounts.user3.address(), 3_900)
            .unwrap()
            .size,
        Quantity::new_int(-2),
        "the reduce-only order tracks the book-fill-reduced position"
    );
    assert_eq!(
        user_state(&suite, perps, accounts.user3.address()).open_order_count,
        1
    );
}

// ------------------------------- child orders --------------------------------

/// A reduce-only order carrying tp/sl child orders keeps them through a shrink
/// and is cleanly removed (no orphan) on cancel.
#[tokio::test]
async fn reduce_only_child_orders_on_resized_order() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let perps = contracts.perps;
    let pair = pair_id();

    deposit(&mut suite, &mut accounts.user2, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user3, perps, DEPOSIT).await;
    deposit(&mut suite, &mut accounts.user4, perps, DEPOSIT).await;

    // user2 opens a 10-unit long.
    rest_limit(
        &mut suite,
        &mut accounts.user3,
        perps,
        &pair,
        -10,
        2_000,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, 10, false).await;

    // user2 rests a reduce-only sell of 10 carrying tp/sl child orders.
    suite
        .execute(
            &mut accounts.user2,
            perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_002),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: true,
                tp: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(2_500),
                    max_slippage: Dimensionless::new_percent(1),
                    size: None,
                }),
                sl: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(1_500),
                    max_slippage: Dimensionless::new_percent(1),
                    size: None,
                }),
            })),
            Coins::new(),
        )
        .await
        .should_succeed();
    assert!(
        order_at(&suite, perps, accounts.user2.address(), 2_002)
            .unwrap()
            .tp
            .is_some(),
        "child orders attached to the resting reduce-only order"
    );

    // Shrink it: user2 sells 6 (as taker) into user4's bid -> long 4, RO -> -4.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        6,
        2_001,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -6, false).await;
    let shrunk = order_at(&suite, perps, accounts.user2.address(), 2_002).unwrap();
    assert_eq!(shrunk.size, Quantity::new_int(-4));
    assert!(
        shrunk.tp.is_some() && shrunk.sl.is_some(),
        "child orders are preserved through a shrink"
    );

    // Close to flat: the reduce-only order (with its children) is cleanly removed.
    rest_limit(
        &mut suite,
        &mut accounts.user4,
        perps,
        &pair,
        4,
        1_999,
        false,
    )
    .await;
    market_fill(&mut suite, &mut accounts.user2, perps, &pair, -4, false).await;
    assert_eq!(
        position(&suite, perps, accounts.user2.address(), &pair),
        None
    );
    assert!(
        open_orders(&suite, perps, accounts.user2.address()).is_empty(),
        "no orphan order or child left after cancel"
    );
}
