use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::Uint128,
    dango_order_book::{
        ChildOrder, Dimensionless, LiquidityDepthResponse, OrderId, OrderKind, OrderPersisted,
        Quantity, QueryOrdersByUserResponseItem, TimeInForce, UsdPrice, UsdValue,
    },
    dango_primitives::{
        Addressable, CheckedContractEvent, Coins, Inner, JsonDeExt, QuerierExt, ResultExt,
        SearchEvent, btree_map, btree_set,
    },
    dango_testing::{TestOption, TestSuiteNaive, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, OrderFilled, PairParam, Param, RateSchedule, UserState},
    },
    std::collections::BTreeMap,
};

/// Covers: deposit, market full fill, withdraw success, withdraw fail.
///
/// | Step | Action                              | Key numbers                                                   | Assert                                                       |
/// | ---- | ----------------------------------- | ------------------------------------------------------------- | ------------------------------------------------------------ |
/// | 1    | Trader starts with margin = $10,000 | —                                                             | `user_state.margin = $10,000`                                |
/// | 2    | Maker A places ask: 10 ETH @ $2,000 | resting on book                                               | ask exists in ASKS                                           |
/// | 3    | Trader market buys 10 ETH           | fee = 10 × $2,000 × 0.1% = $20                                | position: 10 ETH long @ $2,000; margin = $9,980; ask removed |
/// | 4    | Trader withdraws $7,000             | equity=$9,980, used_IM=10×$2,000×10%=$2,000, available=$7,980 | succeeds; margin = $2,980                                    |
/// | 5    | Trader withdraws $2,000             | available = $2,980 - $2,000 = $980                            | fails: "exceeds available margin"                            |
#[tokio::test]
async fn trading_lifecycle() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    // Register oracle prices: ETH = $2,000, USDC = $1.
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Step 1: Trader (user1) deposits $10,000 USDC.
    // USDC has 6 decimals, so $10,000 = 10_000_000_000 base units.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Verify trader's margin = $10,000.
    let state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(state.margin, UsdValue::new_int(10_000));

    // -------------------------------------------------------------------------
    // Step 2: Maker (user2) deposits $10,000 USDC and places ask: 10 ETH @ $2,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10), // sell / ask
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify ask exists on the book.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    assert_eq!(orders.len(), 1, "maker should have 1 ask");

    // -------------------------------------------------------------------------
    // Step 3: Trader market buys 10 ETH.
    // Fee = 10 * $2,000 * 0.1% = $20.  Margin after = $10,000 - $20 = $9,980.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10), // buy
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify position and margin.
    let state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let pos = state
        .positions
        .get(&pair)
        .expect("should have ETH position");

    assert_eq!(pos.size, Quantity::new_int(10), "should be 10 ETH long");
    assert_eq!(pos.entry_price, UsdPrice::new_int(2_000));
    assert_eq!(
        state.margin,
        UsdValue::new_int(9_980),
        "margin should be $9,980 after $20 fee"
    );

    // Maker's ask should be removed.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    assert!(
        orders.is_empty(),
        "maker ask should be fully filled and removed"
    );

    // -------------------------------------------------------------------------
    // Step 4: Trader withdraws $7,000 (should succeed).
    // equity = $9,980, used IM = 10 * $2,000 * 10% = $2,000, available = $7,980.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Withdraw {
                amount: UsdValue::new_int(7_000),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify margin = $9,980 - $7,000 = $2,980.
    let state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(state.margin, UsdValue::new_int(2_980));

    // -------------------------------------------------------------------------
    // Step 5: Trader withdraws $2,000 (should fail).
    // available = $2,980 - IM($2,000) = $980 < $2,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Withdraw {
                amount: UsdValue::new_int(2_000),
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error("exceeds available margin");
}

/// Covers: limit partial fill (rest persists to book), cancel order.
///
/// | Step | Action                              | Key numbers                        | Assert                                                                                                                                                 |
/// | ---- | ----------------------------------- | ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
/// | 1    | Trader margin = $10,000             | —                                  | —                                                                                                                                                      |
/// | 2    | Maker A places ask: 5 ETH @ $2,000  | —                                  | —                                                                                                                                                      |
/// | 3    | Trader limit buys 10 ETH @ $2,000   | 5 filled vs maker, 5 rests as bid  | position: 5 ETH long @ $2,000; fee = $10; margin = $9,990; reserved_margin = 5x$2,000x10% = $1,000 (for resting 5); open_order_count = 1; bid on book  |
/// | 4    | Trader cancels the resting order    | —                                  | reserved_margin = $0; open_order_count = 0; bid removed from book; position unchanged (still 5 ETH)                                                    |
#[tokio::test]
async fn limit_order_partial_fill_and_cancel() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    // Register oracle prices: ETH = $2,000, USDC = $1.
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Step 1: Trader (user1) deposits $10,000 USDC.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 2: Maker (user2) deposits $10,000 USDC and places ask: 5 ETH @ $2,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-5), // sell / ask 5 ETH
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Trader limit buys 10 ETH @ $2,000 (NOT post_only).
    // 5 fill against maker's ask, 5 rest as a bid on the book.
    // Fee = 5 * $2,000 * 0.1% = $10 (taker fee on the filled portion).
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10), // buy 10 ETH
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify position: 5 ETH long @ $2,000, margin = $9,990.
    let state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let pos = state
        .positions
        .get(&pair)
        .expect("should have ETH position");

    assert_eq!(pos.size, Quantity::new_int(5), "should be 5 ETH long");
    assert_eq!(pos.entry_price, UsdPrice::new_int(2_000));
    assert_eq!(
        state.margin,
        UsdValue::new_int(9_990),
        "margin should be $9,990 after $10 fee"
    );

    // Verify reserved_margin = $1,000 for the resting 5 ETH.
    assert_eq!(
        state.reserved_margin,
        UsdValue::new_int(1_000),
        "reserved_margin should be $1,000 for 5 resting ETH"
    );

    // Verify open_order_count = 1.
    assert_eq!(state.open_order_count, 1, "should have 1 open order");

    // Verify bid exists on the book.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();

    assert_eq!(orders.len(), 1, "trader should have 1 resting bid");

    // -------------------------------------------------------------------------
    // Step 4: Trader cancels the resting order.
    // -------------------------------------------------------------------------

    let order_id = *orders.keys().next().unwrap();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::CancelOrder(
                perps::CancelOrderRequest::One(order_id),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify reserved_margin = $0 and open_order_count = 0.
    let state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(
        state.reserved_margin,
        UsdValue::ZERO,
        "reserved_margin should be $0 after cancel"
    );
    assert_eq!(state.open_order_count, 0, "should have 0 open orders");

    // Verify bid removed from book.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();

    assert!(orders.is_empty(), "orders should be empty after cancel");

    // Verify position unchanged: still 5 ETH long @ $2,000.
    let pos = state
        .positions
        .get(&pair)
        .expect("should still have ETH position");

    assert_eq!(pos.size, Quantity::new_int(5), "should still be 5 ETH long");
    assert_eq!(pos.entry_price, UsdPrice::new_int(2_000));
}

/// Verify that liquidity depth bookkeeping tracks resting orders correctly
/// across four code paths: order placement, self-trade prevention (EXPIRE_MAKER),
/// fill, and cancel-all.
#[tokio::test]
async fn liquidity_depth_tracking() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Configure pair with a $100 bucket size for depth tracking.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: default_param(),
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        bucket_sizes: btree_set! { UsdPrice::new_int(100) },
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Deposit margin for both users.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    let query_depth = |suite: &TestSuiteNaive| -> LiquidityDepthResponse {
        suite
            .query_wasm_smart(
                contracts.perps,
                perps::QueryLiquidityDepthRequest {
                    pair_id: pair.clone(),
                    bucket_size: UsdPrice::new_int(100),
                    limit: None,
                },
            )
            .should_succeed()
    };

    // -------------------------------------------------------------------------
    // Step 1: Initial state — no depth.
    // -------------------------------------------------------------------------

    let depth = query_depth(&suite);
    assert!(depth.bids.is_empty(), "bids should be empty initially");
    assert!(depth.asks.is_empty(), "asks should be empty initially");

    // -------------------------------------------------------------------------
    // Step 2: user1 places ask: sell 3 ETH @ $2,000 (post_only).
    // This order will be STP-cancelled when user1 later submits a buy.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-3),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    let depth = query_depth(&suite);

    assert!(depth.bids.is_empty(), "bids should still be empty");
    assert_eq!(depth.asks.len(), 1, "asks should have 1 bucket");

    let ask_bucket = depth.asks.get(&UsdPrice::new_int(2_000)).unwrap();

    assert_eq!(ask_bucket.size, Quantity::new_int(3), "ask size = 3");
    assert_eq!(
        ask_bucket.notional,
        UsdValue::new_int(6_000),
        "ask notional = $6,000"
    );

    // -------------------------------------------------------------------------
    // Step 3: user2 places ask: sell 5 ETH @ $2,000 (post_only).
    // Ask depth accumulates: 3 + 5 = 8 ETH.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    let depth = query_depth(&suite);

    assert!(depth.bids.is_empty(), "bids should still be empty");
    assert_eq!(depth.asks.len(), 1, "asks should have 1 bucket");

    let ask_bucket = depth.asks.get(&UsdPrice::new_int(2_000)).unwrap();

    assert_eq!(ask_bucket.size, Quantity::new_int(8), "ask size = 8");
    assert_eq!(
        ask_bucket.notional,
        UsdValue::new_int(16_000),
        "ask notional = $16,000"
    );

    // -------------------------------------------------------------------------
    // Step 4: user1 limit buys 10 ETH @ $2,000 (NOT post_only).
    // The matching engine walks the ask book:
    //   - user1's own 3 ETH ask → EXPIRE_MAKER cancels it (ask depth −3).
    //     Taker does NOT consume these; remaining taker size stays 10.
    //   - user2's 5 ETH ask → fills 5 (ask depth −5). Remaining = 5.
    //   - No more asks → 5 ETH rests as bid (bid depth +5).
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    let depth = query_depth(&suite);

    assert!(
        depth.asks.is_empty(),
        "asks should be empty after STP + fill"
    );
    assert_eq!(depth.bids.len(), 1, "bids should have 1 bucket");

    let bid_bucket = depth.bids.get(&UsdPrice::new_int(2_000)).unwrap();

    assert_eq!(bid_bucket.size, Quantity::new_int(5), "bid size = 5");
    assert_eq!(
        bid_bucket.notional,
        UsdValue::new_int(10_000),
        "bid notional = $10,000"
    );

    // -------------------------------------------------------------------------
    // Step 5: Cancel all user1's orders → bid depth drops to zero.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::CancelOrder(
                perps::CancelOrderRequest::All,
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    let depth = query_depth(&suite);

    assert!(
        depth.bids.is_empty(),
        "bids should be empty after cancel-all"
    );
    assert!(
        depth.asks.is_empty(),
        "asks should be empty after cancel-all"
    );
}

/// Covers: protocol treasury fees accumulate across multiple fills.
///
/// This is a regression test for a bug where `settle_pnls` used
/// `maker_states.entry(protocol_treasury).or_default()` — which created a
/// blank `UserState` every call, losing previously accumulated fees.
///
/// After the fix, protocol fees are stored in `State::treasury` (analogous
/// to `State::insurance_fund`) and accumulate correctly.
///
/// | Step | Action                                 | Assert                                                  |
/// | ---- | -------------------------------------- | ------------------------------------------------------- |
/// | 1    | Configure: protocol_fee_rate = 20%     | —                                                       |
/// | 2    | Maker places ask 10 ETH @ $2,000       | —                                                       |
/// | 3    | Taker market buys 10 ETH               | fee=$20, protocol=$4 → State.treasury == $4             |
/// | 4    | Maker places another ask 10 ETH @ $2k  | —                                                       |
/// | 5    | Taker market buys 10 ETH               | another $4 → State.treasury == $8 (accumulated!)        |
#[tokio::test]
async fn protocol_fee_accumulates_across_fills() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Configure: set protocol_fee_rate = 20%.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    protocol_fee_rate: Dimensionless::new_percent(20),
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => default_pair_param(),
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Deposit for maker (user2) and taker (user1).
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // --- Fill 1: Maker places ask 10 ETH @ $2,000, taker market buys ---

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // fee = 10 * $2,000 * 0.1% = $20
    // protocol_fee = $20 * 20% = $4
    let global_state: perps::State = suite
        .query_wasm_smart(contracts.perps, perps::QueryStateRequest {})
        .should_succeed();

    assert_eq!(
        global_state.treasury,
        UsdValue::new_int(4),
        "treasury should be $4 after first fill"
    );

    // --- Fill 2: Maker places another ask 10 ETH @ $2,000, taker buys ---

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // another $4 → total should be $8 (accumulated, not overwritten!)
    let global_state: perps::State = suite
        .query_wasm_smart(contracts.perps, perps::QueryStateRequest {})
        .should_succeed();

    assert_eq!(
        global_state.treasury,
        UsdValue::new_int(8),
        "treasury should be $8 after second fill (accumulated, not overwritten)"
    );
}

/// Negative maker fee (rebate): maker is paid on every fill.
///
/// | Step | Action                                          | Key numbers                                                        | Assert                                   |
/// | ---- | ----------------------------------------------- | ------------------------------------------------------------------ | ---------------------------------------- |
/// | 1    | Configure: taker=3 bps, maker=-1 bps, proto=20%| —                                                                  | config accepted                          |
/// | 2    | Deposit $100,000 each for taker and maker       | —                                                                  | margins = $100,000                       |
/// | 3    | Maker places post_only ask: 50 ETH @ $2,000    | resting on book                                                    | ask exists                               |
/// | 4    | Taker market buys 50 ETH                        | notional=$100k, taker fee=$30, maker fee=-$10                      | taker margin=$99,970; maker margin=$100,010 |
/// | 5    | Check treasury                                  | proto: taker $6 + maker -$2 = $4                                   | treasury=$4                              |
#[tokio::test]
async fn negative_maker_fee_rebate_lifecycle() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Step 1: Configure taker = 3 bps, maker = -1 bps, protocol_fee = 20%.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    taker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(300), // 3 bps
                        ..Default::default()
                    },
                    maker_fee_rates: RateSchedule {
                        base: Dimensionless::new_raw(-100), // -1 bps (rebate)
                        ..Default::default()
                    },
                    protocol_fee_rate: Dimensionless::new_percent(20),
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair.clone() => default_pair_param(),
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 2: Deposit $100,000 each for maker (user2) and taker (user1).
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Maker (user2) places post_only ask: sell 50 ETH @ $2,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-50),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 4: Taker (user1) market buys 50 ETH.
    //
    // Notional = 50 × $2,000 = $100,000
    // Taker fee = $100,000 × 3 bps = $30
    // Maker fee = $100,000 × (-1 bps) = -$10 (rebate)
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(50),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 4 assertions: check margins.
    // -------------------------------------------------------------------------

    // Taker: $100,000 - $30 fee = $99,970.
    let taker_state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(
        taker_state.margin,
        UsdValue::new_int(99_970),
        "taker margin should be $99,970 after paying $30 fee"
    );

    // Maker: $100,000 + $10 rebate = $100,010.
    let maker_state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(
        maker_state.margin,
        UsdValue::new_int(100_010),
        "maker margin should be $100,010 after receiving $10 rebate"
    );

    // -------------------------------------------------------------------------
    // Step 5: Check treasury.
    //
    // Protocol fee from taker: $30 × 20% = $6
    // Protocol fee from maker: -$10 × 20% = -$2
    // Net treasury = $4
    // -------------------------------------------------------------------------

    let global_state: perps::State = suite
        .query_wasm_smart(contracts.perps, perps::QueryStateRequest {})
        .should_succeed();

    assert_eq!(
        global_state.treasury,
        UsdValue::new_int(4),
        "treasury should be $4 (taker $6 + maker -$2)"
    );
}

/// IOC limit order: partial fill with unfilled remainder cancelled (not stored).
///
/// | Step | Action                                        | Assert                                          |
/// |------|-----------------------------------------------|-------------------------------------------------|
/// | 1    | Both users deposit $10,000 USDC               | margin established                              |
/// | 2    | Maker (user2) places post-only ask: 5 ETH     | ask on book                                     |
/// | 3    | Taker (user1) IOC limit buy 10 ETH @ $2,000   | 5 fill, 5 cancelled                             |
/// | 4    | Verify taker state                            | position=5, open_order_count=0, reserved=$0     |
#[tokio::test]
async fn ioc_limit_order_partial_fill() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Step 1: Both users deposit $10,000 USDC.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 2: Maker (user2) places post-only ask: sell 5 ETH @ $2,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Taker (user1) IOC limit buy 10 ETH @ $2,000.
    // 5 fill against maker, 5 unfilled → cancelled (IOC).
    // Fee = 5 × $2,000 × 0.1% = $10.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::ImmediateOrCancel,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 4: Verify taker state.
    // -------------------------------------------------------------------------

    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state
        .positions
        .get(&pair)
        .expect("should have ETH position");

    assert_eq!(pos.size, Quantity::new_int(5), "should be 5 ETH long");
    assert_eq!(pos.entry_price, UsdPrice::new_int(2_000));
    assert_eq!(
        state.margin,
        UsdValue::new_int(9_990),
        "margin should be $9,990 after $10 taker fee"
    );

    // IOC: no resting order.
    assert_eq!(
        state.reserved_margin,
        UsdValue::ZERO,
        "reserved_margin should be $0 (IOC cancelled unfilled)"
    );
    assert_eq!(state.open_order_count, 0, "should have 0 open orders");

    // Verify no resting orders on book.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();

    assert!(orders.is_empty(), "IOC taker should have no resting orders");
}

/// IOC limit order with zero fills errors out.
///
/// | Step | Action                                          | Assert                         |
/// |------|-------------------------------------------------|--------------------------------|
/// | 1    | Taker deposits $10,000 USDC                     | margin established             |
/// | 2    | Taker IOC limit buy 10 ETH @ $1,900 (empty book)| error: no liquidity            |
#[tokio::test]
async fn ioc_limit_order_no_fill_rejected() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Step 1: Deposit.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Step 2: IOC limit buy against empty book → should fail.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_900),
                    time_in_force: TimeInForce::ImmediateOrCancel,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_fail_with_error("no liquidity at acceptable price");
}

// ==================== market-order slippage cap ==============================
//
// Set up a pair with `max_market_slippage = 5%` and verify the cap is
// enforced at submission for market orders and TP/SL child orders while
// leaving limit orders untouched.

/// Set up a suite with oracle = $2,000 and a pair configured with 5%
/// `max_market_slippage`. user1 is funded with $10,000.
macro_rules! setup_slippage_cap_suite {
    () => {{
        let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
        let pair = pair_id();

        register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

        suite
            .execute(
                &mut accounts.owner,
                contracts.perps,
                &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                    param: default_param(),
                    pair_params: btree_map! {
                        pair.clone() => PairParam {
                            max_market_slippage: Dimensionless::new_permille(50), // 5%
                            ..default_pair_param()
                        },
                    },
                }),
                Coins::new(),
            )
            .await
            .should_succeed();

        suite
            .execute(
                &mut accounts.user1,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
            )
            .await
            .should_succeed();

        (suite, accounts, contracts, pair)
    }};
}

/// Market order with slippage exactly at the pair cap is accepted at
/// submission. (It fails later for lack of liquidity — that's fine; the
/// point of the test is the submission-time check.)
#[tokio::test]
async fn slippage_cap_market_at_cap_accepted_at_submission() {
    let (mut suite, mut accounts, contracts, pair) = setup_slippage_cap_suite!();

    // max_slippage = 5% (exactly the cap). The order passes the cap
    // check, then fails downstream because the book is empty.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_permille(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_fail_with_error("no liquidity at acceptable price");
}

/// Market order with slippage just above the pair cap is rejected at
/// submission with the cap error (not "no liquidity"). This proves the
/// cap check runs before matching.
#[tokio::test]
async fn slippage_cap_market_above_cap_rejected() {
    let (mut suite, mut accounts, contracts, pair) = setup_slippage_cap_suite!();

    // max_slippage = 6% against 5% cap.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_permille(60),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_fail_with_error("exceeds the pair cap");
}

/// TP/SL child order slippage is capped by the same per-pair parameter
/// as the top-level market order.
#[tokio::test]
async fn slippage_cap_tpsl_child_order_above_cap_rejected() {
    let (mut suite, mut accounts, contracts, pair) = setup_slippage_cap_suite!();

    // Parent market order with slippage within cap, but the TP child
    // order's slippage (6%) is above the cap.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_permille(10), // 1%, within cap
                },
                reduce_only: false,
                tp: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(2_500),
                    max_slippage: Dimensionless::new_permille(60), // 6%, above cap
                    size: None,
                }),
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_fail_with_error("exceeds the pair cap");
}

/// Limit orders do not carry `max_slippage` and are unaffected by the
/// market-order slippage cap. Their `limit_price` is bounded instead by
/// `max_limit_price_deviation` (PR1 banding).
#[tokio::test]
async fn slippage_cap_does_not_affect_limit_orders() {
    let (mut suite, mut accounts, contracts, pair) = setup_slippage_cap_suite!();

    // Limit buy at oracle = $2,000. Always within the 10% band of
    // default_pair_param (via new_mock → 50% band). Cap is irrelevant
    // here because limit orders don't have a max_slippage field.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();
}

/// A taker that crosses two resting maker orders emits four `OrderFilled`
/// events — two per match — and the two events of a given match share a
/// `fill_id`. Successive matches use consecutive fill ids, and
/// `NEXT_FILL_ID` in storage advances by one per match.
#[tokio::test]
async fn fill_id_is_shared_across_match_sides_and_increments_per_match() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Two different maker users so we also exercise the maker-states map.
    for user in [&mut accounts.user1, &mut accounts.user2] {
        suite
            .execute(
                user,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
            )
            .await
            .should_succeed();
    }
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Two resting asks at different prices. The taker's market buy will
    // sweep the cheaper one first, then fill the rest at the higher price —
    // two distinct matches, four `OrderFilled` events total.
    for (maker, price) in [(&mut accounts.user1, 2_000), (&mut accounts.user2, 2_010)] {
        suite
            .execute(
                maker,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(
                    perps::SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(-2), // ask
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(price),
                            time_in_force: TimeInForce::PostOnly,
                            client_order_id: None,
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    },
                )),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    // Taker market buy crosses both asks.
    let events = suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(4),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    let fills = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_filled")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderFilled>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(fills.len(), 4, "two matches × two sides = four fills");

    for fill in &fills {
        assert!(
            fill.fill_id.is_some(),
            "every matched fill must carry a fill id"
        );
    }

    // Matching iterates makers in price-time priority (best price first).
    // Given the current iteration order, the event stream pairs are
    // (fills[0] taker, fills[1] maker) and (fills[2] taker, fills[3] maker).
    let match1 = fills[0].fill_id.unwrap();
    assert_eq!(
        fills[1].fill_id.unwrap(),
        match1,
        "the two sides of a match share one fill id"
    );

    let match2 = fills[2].fill_id.unwrap();
    assert_eq!(
        fills[3].fill_id.unwrap(),
        match2,
        "the two sides of a match share one fill id"
    );

    assert_ne!(match1, match2, "distinct matches have distinct fill ids");
    assert_eq!(
        match2.into_inner(),
        match1.into_inner() + 1,
        "fill ids increment by one per match"
    );

    // None of the participants held a prior position in this pair, so
    // funding is zero on every leg — but the field must still be `Some`
    // (the `None` shape is reserved for pre-v0.17.0 fills).
    for fill in &fills {
        assert_eq!(
            fill.realized_funding,
            Some(UsdValue::ZERO),
            "v0.17.0+ OrderFilled events always carry a Some(realized_funding); \
             with no prior position it must be Some(ZERO)"
        );
    }

    // The two post-fill fields. Event order per match is (taker, maker), so
    // fills are [taker@1, maker1, taker@2, maker2]. Values derived from the
    // trade intent (taker buys 4 across two 2-lot makers), not the code:
    //
    // - resulting position: the taker ends long 2 after match 1 and long 4
    //   after match 2; each maker starts flat and ends short 2.
    // - order remainder: the taker's buy-4 has 2 left after match 1 and 0
    //   after match 2; each maker's 2-lot ask is fully consumed (0 left).
    assert_eq!(
        fills
            .iter()
            .map(|f| f.remaining_position_size)
            .collect::<Vec<_>>(),
        vec![
            Some(Quantity::new_int(2)),
            Some(Quantity::new_int(-2)),
            Some(Quantity::new_int(4)),
            Some(Quantity::new_int(-2)),
        ],
        "each OrderFilled must report the position size resulting from that fill"
    );
    assert_eq!(
        fills
            .iter()
            .map(|f| f.remaining_order_size)
            .collect::<Vec<_>>(),
        vec![
            Some(Quantity::new_int(2)),
            Some(Quantity::ZERO),
            Some(Quantity::ZERO),
            Some(Quantity::ZERO),
        ],
        "each OrderFilled must report the order's remaining size after that fill"
    );
}

/// A fill that closes and then flips a position reports the resulting *signed*
/// position size and the order's post-fill remainder. Complements
/// `fill_id_is_shared_across_match_sides_and_increments_per_match`, which only
/// exercises position-opening fills.
///
/// Trader (user3) opens long 10, then sells 15 in a single match: this closes
/// the 10 long and opens a 5 short — the `decompose_fill` flip example
/// documented on `OrderFilled`. The taker fill must report
/// `remaining_position_size = Some(-5)` and, since the sell fully fills against
/// a 15-lot bid, `remaining_order_size = Some(0)`.
#[tokio::test]
async fn order_filled_reports_resulting_position_size_on_flip() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    for user in [
        &mut accounts.user1,
        &mut accounts.user2,
        &mut accounts.user3,
    ] {
        suite
            .execute(
                user,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
            )
            .await
            .should_succeed();
    }

    // Open: user1 rests a 10-lot ask; user3 market-buys 10 → long 10.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Flip: user2 rests a 15-lot bid; user3 market-sells 15 → closes the 10
    // long and opens a 5 short.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(15),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();
    let flip_events = suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-15),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    let taker_fill = flip_events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_filled")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderFilled>().unwrap())
        .find(|f| f.user == accounts.user3.address())
        .expect("a taker order_filled event for user3");

    // The documented `decompose_fill` flip: long 10 selling 15 closes 10 and
    // opens 5 on the short side.
    assert_eq!(taker_fill.fill_size, Quantity::new_int(-15));
    assert_eq!(taker_fill.closing_size, Quantity::new_int(-10));
    assert_eq!(taker_fill.opening_size, Quantity::new_int(-5));
    // Resulting position: flipped from long 10 to short 5.
    assert_eq!(
        taker_fill.remaining_position_size,
        Some(Quantity::new_int(-5)),
        "selling 15 while long 10 must leave the position short 5"
    );
    // The 15-lot sell fully fills against the 15-lot bid → nothing left.
    assert_eq!(
        taker_fill.remaining_order_size,
        Some(Quantity::ZERO),
        "a fully-filled order must report zero remaining size"
    );
}

/// A partially-filled maker's `OrderFilled` must report the resting order's
/// post-fill remainder, and that remainder must equal the order left on the
/// book. Every prior fill test consumes its makers in full, so the non-zero
/// maker-side `remaining_order_size` was never exercised.
///
/// user1 rests a 10-lot ask; user3 market-buys 4. By intent: the taker is
/// fully filled (remainder 0, position long 4), the maker keeps a 6-lot ask
/// on the book (remainder -6, position short 4).
#[tokio::test]
async fn order_filled_reports_maker_remainder_on_partial_fill() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    for user in [&mut accounts.user1, &mut accounts.user3] {
        suite
            .execute(
                user,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
            )
            .await
            .should_succeed();
    }

    // user1 rests a 10-lot ask at $2,000.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // user3 market-buys 4, partially consuming the resting ask.
    let events = suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(4),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    let fills = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_filled")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderFilled>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(fills.len(), 2, "one match → taker and maker OrderFilled");

    let taker_fill = fills
        .iter()
        .find(|f| f.user == accounts.user3.address())
        .expect("a taker fill for user3");
    assert_eq!(taker_fill.is_maker, Some(false));
    assert_eq!(
        taker_fill.remaining_order_size,
        Some(Quantity::ZERO),
        "the 4-lot buy is fully filled"
    );
    assert_eq!(
        taker_fill.remaining_position_size,
        Some(Quantity::new_int(4))
    );

    let maker_fill = fills
        .iter()
        .find(|f| f.user == accounts.user1.address())
        .expect("a maker fill for user1");
    assert_eq!(maker_fill.is_maker, Some(true));
    assert_eq!(
        maker_fill.remaining_order_size,
        Some(Quantity::new_int(-6)),
        "the 10-lot ask keeps 6 lots after filling 4"
    );
    assert_eq!(
        maker_fill.remaining_position_size,
        Some(Quantity::new_int(-4))
    );

    // The event's remainder must equal the order actually left on the book.
    let maker_orders = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(
        maker_orders.len(),
        1,
        "the partially-filled ask still rests"
    );
    let resting = maker_orders.values().next().unwrap();
    assert_eq!(
        Some(resting.size),
        maker_fill.remaining_order_size,
        "event remainder must match the resized resting order"
    );

    // And the events' position sizes must equal the durable user states.
    for (user, expected) in [
        (accounts.user1.address(), Quantity::new_int(-4)),
        (accounts.user3.address(), Quantity::new_int(4)),
    ] {
        let state = suite
            .query_wasm_smart(contracts.perps, perps::QueryUserStateRequest { user })
            .should_succeed()
            .unwrap();
        assert_eq!(state.positions[&pair].size, expected);
    }
}

/// A GTC taker whose remainder rests on the book must report that remainder
/// on its last fill, and the value must equal both the `OrderPersisted`
/// event's size and the durable resting order. No prior test asserts
/// `OrderPersisted` at all.
///
/// user1 rests a 5-lot ask; user3 submits a GTC limit buy for 10 at the same
/// price. By intent: 5 fill (position long 5), 5 rest as a bid.
#[tokio::test]
async fn order_filled_taker_remainder_matches_persisted_order() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    for user in [&mut accounts.user1, &mut accounts.user3] {
        suite
            .execute(
                user,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
            )
            .await
            .should_succeed();
    }

    // user1 rests a 5-lot ask at $2,000.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // user3 GTC-buys 10 at $2,000: 5 fill, 5 rest.
    let events = suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    let fills = events
        .clone()
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_filled")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderFilled>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(fills.len(), 2, "one match → taker and maker OrderFilled");

    let taker_fill = fills
        .iter()
        .find(|f| f.user == accounts.user3.address())
        .expect("a taker fill for user3");
    assert_eq!(
        taker_fill.remaining_order_size,
        Some(Quantity::new_int(5)),
        "the 10-lot buy has 5 left after consuming the 5-lot ask"
    );
    assert_eq!(
        taker_fill.remaining_position_size,
        Some(Quantity::new_int(5))
    );

    let maker_fill = fills
        .iter()
        .find(|f| f.user == accounts.user1.address())
        .expect("a maker fill for user1");
    assert_eq!(
        maker_fill.remaining_order_size,
        Some(Quantity::ZERO),
        "the 5-lot ask is fully consumed"
    );

    // The remainder rests: OrderPersisted must carry the same size the
    // taker's fill reported as remaining.
    let persisted = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_persisted")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderPersisted>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        persisted.len(),
        1,
        "the unfilled remainder rests on the book"
    );
    assert_eq!(persisted[0].user, accounts.user3.address());
    assert_eq!(persisted[0].limit_price, UsdPrice::new_int(2_000));
    assert_eq!(
        Some(persisted[0].size),
        taker_fill.remaining_order_size,
        "OrderPersisted size must equal the taker fill's remaining_order_size"
    );

    // Durable cross-check: the resting bid on the book carries the same size,
    // and the maker's book is empty.
    let taker_orders = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user3.address(),
            },
        )
        .should_succeed();
    assert_eq!(taker_orders.len(), 1);
    let resting = taker_orders.values().next().unwrap();
    assert_eq!(resting.size, Quantity::new_int(5));
    assert_eq!(resting.limit_price, UsdPrice::new_int(2_000));

    let maker_orders = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert!(
        maker_orders.is_empty(),
        "the maker's ask was fully consumed"
    );
}

/// Bug reproduction: sell-side market order with partial fill is incorrectly
/// rejected due to a sign-sensitive comparison at `submit_order.rs:579`.
///
/// The check `unfilled < fillable_size` works for buys (positive quantities)
/// but is inverted for sells (negative quantities):
///
///   Sell 10: fillable_size = -10, after 5 filled unfilled = -5.
///   -5 < -10 → false → ensure! fires → order rejected.
///
/// This test places a maker bid for 5 ETH, then submits a market sell for 10.
/// The 5 available should fill and the remaining 5 should be discarded.
/// With the bug, the entire order is rejected.
#[tokio::test]
async fn sell_side_market_order_partial_fill() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Step 1: Deposit margin for both users.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 2: Maker (user2) places bid: buy 5 ETH @ $2,000 (post-only).
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5), // buy / bid
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Taker (user1) market sells 10 ETH.
    // Only 5 ETH of liquidity exists → should partially fill 5 and discard
    // the remaining 5.
    //
    // BUG: the comparison `unfilled < fillable_size` evaluates as
    //   -5 < -10 → false, so ensure! rejects the order.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-10), // sell 10 ETH
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify taker has a 10 ETH short position from the 5 that filled.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state
        .positions
        .get(&pair)
        .expect("taker should have a position after partial fill");

    assert_eq!(
        pos.size,
        Quantity::new_int(-5),
        "taker should be 5 ETH short (partial fill of 10 ETH sell)"
    );

    // Maker's bid should be fully consumed.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    assert!(
        orders.is_empty(),
        "maker bid should be fully filled and removed"
    );
}

/// Mirror of `sell_side_market_order_partial_fill`: a buy-side market order
/// with partial fill works correctly because the comparison
/// `unfilled < fillable_size` is correct for positive quantities.
///
///   Buy 10: fillable_size = 10, after 5 filled unfilled = 5.
///   5 < 10 → true → passes.
#[tokio::test]
async fn buy_side_market_order_partial_fill() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Step 1: Deposit margin for both users.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 2: Maker (user2) places ask: sell 5 ETH @ $2,000 (post-only).
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-5), // sell / ask
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_000),
                    time_in_force: TimeInForce::PostOnly,
                    client_order_id: None,
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 3: Taker (user1) market buys 10 ETH.
    // Only 5 ETH of liquidity exists → should partially fill 5 and discard
    // the remaining 5. This works because 5 < 10 → true.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10), // buy 10 ETH
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify taker has a 5 ETH long position from the partial fill.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state
        .positions
        .get(&pair)
        .expect("taker should have a position after partial fill");

    assert_eq!(
        pos.size,
        Quantity::new_int(5),
        "taker should be 5 ETH long (partial fill of 10 ETH buy)"
    );

    // Maker's ask should be fully consumed.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    assert!(
        orders.is_empty(),
        "maker ask should be fully filled and removed"
    );
}
