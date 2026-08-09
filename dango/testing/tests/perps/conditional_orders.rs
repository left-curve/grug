use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::Uint128,
    dango_order_book::{
        ChildOrder, Dimensionless, OrderId, OrderKind, Quantity, QueryOrdersByUserResponseItem,
        TimeInForce, TriggerDirection, UsdPrice, UsdValue,
    },
    dango_primitives::{
        Addressable, CheckedContractEvent, Coins, Duration, Inner, JsonDeExt, QuerierExt,
        ResultExt, SearchEvent, btree_map,
    },
    dango_testing::{TestOption, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, OrderFilled, PairParam, UserState},
    },
    std::collections::BTreeMap,
};

/// Full lifecycle: deposit → open position → place TP → oracle rises →
/// cron triggers TP → position closed.
#[tokio::test]
async fn conditional_order_tp_triggers_on_price_rise() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Step 1: Trader deposits $10,000 USDC.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Step 2: Maker places ask: 10 ETH @ $2,000.
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

    // Step 3: Trader market buys 10 ETH. Fee = 10 * $2,000 * 0.1% = $20.
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

    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(state.margin, UsdValue::new_int(9_980));
    assert_eq!(
        state.positions.get(&pair).unwrap().size,
        Quantity::new_int(10)
    );

    // Step 4: Trader submits TP: sell 10 @ trigger $2,500 Above, 1% slippage.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-10)),
                trigger_price: UsdPrice::new_int(2_500),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(1),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Step 5: Verify conditional order exists on the position.
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
    assert!(
        pos.conditional_order_above.is_some(),
        "should have a conditional order above (TP)"
    );

    // Step 6: Bidder (user2) places bid: 10 ETH @ $2,500.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_500),
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

    // Step 7: Oracle updated to $2,500.
    register_oracle_prices(&mut suite, &mut accounts, 2_500).await;

    // Step 8: Advance time so perps cron fires (interval = 1 min).
    suite.increase_time(Duration::from_minutes(2)).await;

    // Step 9: Verify trader state — position closed.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        !state.positions.contains_key(&pair),
        "position should be closed after TP triggered"
    );

    // PnL should be positive: 10 * ($2,500 - $2,000) = +$5,000 (minus fees).
    // Margin started at $9,980, so should be > $14,000.
    assert!(
        state.margin > UsdValue::new_int(14_000),
        "margin should reflect positive PnL: got {:?}",
        state.margin
    );

    // Step 10: Position is closed, so conditional orders are gone with it.
    // Verify no positions remain (and thus no conditional orders).
    assert!(
        state.positions.is_empty(),
        "all positions should be gone after TP triggered"
    );
}

/// SL triggers on price drop: deposit → buy → place SL → oracle drops →
/// cron triggers SL → position closed with loss.
#[tokio::test]
async fn conditional_order_sl_triggers_on_price_drop() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Step 1: Trader deposits $10,000, buys 5 ETH @ $2,000.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Maker deposits and places ask.
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

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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

    // Step 2: Trader submits SL: sell 5 @ trigger $1,800 Below, 2% slippage.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-5)),
                trigger_price: UsdPrice::new_int(1_800),
                trigger_direction: TriggerDirection::Below,
                max_slippage: Dimensionless::new_percent(2),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Step 3: Bidder places bid: 5 ETH @ $1,800.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_800),
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

    // Step 4: Oracle drops to $1,800, advance time so perps cron fires.
    register_oracle_prices(&mut suite, &mut accounts, 1_800).await;
    suite.increase_time(Duration::from_minutes(2)).await;

    // Step 5: Verify trader state — position closed, PnL = 5*($1,800-$2,000) = -$1,000.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        !state.positions.contains_key(&pair),
        "position should be closed after SL triggered"
    );
    // Position closed → conditional orders are gone with it.
    assert!(
        state.positions.is_empty(),
        "all positions should be gone after SL triggered"
    );

    // Margin started at $9,990 (after $10 fee), loss of $1,000, minus close fee.
    // Should be roughly $8,980.
    assert!(
        state.margin < UsdValue::new_int(9_000),
        "margin should reflect loss: got {:?}",
        state.margin
    );
}

/// BELOW conditional orders store `!trigger_price` (bitwise-inverted) in the
/// storage key so that ascending iteration yields descending real trigger
/// prices. This means the order closest to the current market price executes
/// first during cron processing.
///
/// This test verifies price-time priority by placing two BELOW stop-losses at
/// different trigger prices ($1,900 and $1,800). Two bids at different prices
/// ($1,790 better, $1,770 worse) sit on the book. When the oracle drops to
/// $1,800 and the cron fires, the $1,900 SL (closer to market) must execute
/// first and consume the better $1,790 bid, leaving the $1,770 bid for the
/// $1,800 SL.
#[tokio::test]
async fn conditional_orders_follow_price_time_priority() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Setup: User1, User3 deposit $10k each. Maker (user2) deposits $100k.
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
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 1: Maker places ask: 10 ETH @ $2,000.
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    // Step 2: User1 market buys 5 ETH → 5 ETH long @ $2,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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
    // Step 3: User3 market buys 5 ETH → 5 ETH long @ $2,000.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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
    // Step 4: User1 places SL: BELOW $1,900, size -5, max_slippage 2%.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-5)),
                trigger_price: UsdPrice::new_int(1_900),
                trigger_direction: TriggerDirection::Below,
                max_slippage: Dimensionless::new_percent(2),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 5: User3 places SL: BELOW $1,800, size -5, max_slippage 2%.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-5)),
                trigger_price: UsdPrice::new_int(1_800),
                trigger_direction: TriggerDirection::Below,
                max_slippage: Dimensionless::new_percent(2),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 6: Maker places two bids at different prices.
    //   - 5 ETH @ $1,790 (better price — consumed by first-to-execute order)
    //   - 5 ETH @ $1,770 (worse price — consumed by second-to-execute order)
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_790),
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
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_770),
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
    // Step 7: Oracle → $1,800, advance time 2 min so cron fires.
    //
    // Both SLs trigger (oracle <= trigger_price for both $1,900 and $1,800).
    // Correct priority (descending real trigger price):
    //   User1's SL ($1,900) executes first → fills against best bid @ $1,790.
    //   User3's SL ($1,800) executes second → fills against next bid @ $1,770.
    //
    // Slippage check: oracle=$1,800, max_slippage=2%, target=$1,800*0.98=$1,764.
    // Both $1,790 and $1,770 are above $1,764 → within tolerance.
    // -------------------------------------------------------------------------

    register_oracle_prices(&mut suite, &mut accounts, 1_800).await;
    suite.increase_time(Duration::from_minutes(2)).await;

    // -------------------------------------------------------------------------
    // Assertions: Both positions closed, both conditional orders consumed.
    // -------------------------------------------------------------------------

    let state_user1: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        !state_user1.positions.contains_key(&pair),
        "User1 position should be closed after SL triggered"
    );
    // Position closed → conditional orders gone with it.
    assert!(
        state_user1.positions.is_empty(),
        "User1 should have no positions (and thus no conditional orders)"
    );

    let state_user3: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user3.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        !state_user3.positions.contains_key(&pair),
        "User3 position should be closed after SL triggered"
    );
    assert!(
        state_user3.positions.is_empty(),
        "User3 should have no positions (and thus no conditional orders)"
    );

    // User1 got the better fill ($1,790) so should have more margin than User3
    // who got the worse fill ($1,770). Both started with the same deposit and
    // position, so the ~$100 PnL difference should be reflected in margins.
    //
    // User1 PnL: 5 * ($1,790 - $2,000) = -$1,050
    // User3 PnL: 5 * ($1,770 - $2,000) = -$1,150
    assert!(
        state_user1.margin > state_user3.margin,
        "User1 margin ({}) should exceed User3 margin ({}) — \
         User1 got the better fill due to price-time priority",
        state_user1.margin,
        state_user3.margin,
    );
}

/// When a conditional order's `compute_submit_order_outcome` fails (e.g. no liquidity on the
/// book for the order's side), the cron now gracefully cancels it with
/// `SlippageExceeded` instead of propagating the error via `?`. Previously
/// the error would abort the entire cron, leaving the failed order stuck
/// retrying every tick and blocking all subsequent conditional orders from
/// processing.
///
/// This test places two BELOW conditional orders: one sell (no bids on book →
/// will fail) and one buy (ask available → will succeed). It verifies that
/// the first order's failure does not prevent the second from executing.
#[tokio::test]
async fn conditional_order_failure_does_not_block_others() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // -------------------------------------------------------------------------
    // Setup: User1, User3 deposit $10k each. Maker (user2) deposits $100k.
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
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 1: Maker places ask: 5 ETH @ $2,000. User1 market buys 5 ETH long.
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

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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
    // Step 2: Maker places bid: 5 ETH @ $2,000. User3 market sells 5 ETH short.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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
                size: Quantity::new_int(-5),
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

    // Verify positions: User1 = 5 long, User3 = 5 short.
    let state_user1: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(
        state_user1.positions.get(&pair).unwrap().size,
        Quantity::new_int(5),
        "User1 should be 5 ETH long"
    );

    let state_user3: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user3.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(
        state_user3.positions.get(&pair).unwrap().size,
        Quantity::new_int(-5),
        "User3 should be 5 ETH short"
    );

    // -------------------------------------------------------------------------
    // Step 3: User1 places SL: BELOW $1,900, size -5 (sell). No bids will be
    // on book at trigger time → compute_submit_order_outcome will fail.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-5)),
                trigger_price: UsdPrice::new_int(1_900),
                trigger_direction: TriggerDirection::Below,
                max_slippage: Dimensionless::new_percent(2),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 4: User3 places closing order: BELOW $1,800, size +5 (buy to close
    // short). Maker will place an ask for this to fill against.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(5)),
                trigger_price: UsdPrice::new_int(1_800),
                trigger_direction: TriggerDirection::Below,
                max_slippage: Dimensionless::new_percent(2),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // -------------------------------------------------------------------------
    // Step 5: Maker places ask: 5 ETH @ $1,800 (liquidity for User3's buy).
    // No bids are placed — User1's sell will have nothing to fill against.
    // -------------------------------------------------------------------------

    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_800),
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
    // Step 6: Oracle → $1,800, advance time 2 min so cron fires.
    //
    // Processing order (BELOW, descending real trigger price):
    //   1. User1's SL ($1,900) triggers → sell → no bids → fails → graceful
    //      SlippageExceeded cancel → cron continues.
    //   2. User3's order ($1,800) triggers → buy → fills against ask @ $1,800
    //      → succeeds.
    // -------------------------------------------------------------------------

    register_oracle_prices(&mut suite, &mut accounts, 1_800).await;
    suite.increase_time(Duration::from_minutes(2)).await;

    // -------------------------------------------------------------------------
    // Assertions
    // -------------------------------------------------------------------------

    // User1: position unchanged (sell failed), conditional order cancelled (not stuck).
    let state_user1: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(
        state_user1.positions.get(&pair).unwrap().size,
        Quantity::new_int(5),
        "User1 should still be 5 ETH long (sell had no liquidity)"
    );

    // User3: position closed (short covered), conditional order consumed.
    let state_user3: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user3.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        !state_user3.positions.contains_key(&pair),
        "User3 short should be closed (buy filled against ask @ $1,800)"
    );
    // Both users' conditional orders should be cleared from their positions.
    // User1: position still exists (sell failed), but conditional order should be
    // canceled (graceful SlippageExceeded removal).
    let pos_user1 = state_user1
        .positions
        .get(&pair)
        .expect("User1 should still have a position");
    assert!(
        pos_user1.conditional_order_below.is_none(),
        "User1 conditional_order_below should be None after graceful cancel"
    );

    // User3: position is closed (already asserted above), so no conditional
    // orders to check — absence of position is sufficient.
}

/// Regression: when a triggered conditional order's `execute_order` call fails
/// *after* self-trade prevention has decremented the taker's in-memory
/// `open_order_count` / `reserved_margin`, `process_triggered_order` must
/// discard the partial mutations. Otherwise the user's saved `UserState` drifts
/// out of sync with the order book: `open_order_count` is too low while the
/// resting order is still on the book, and a later fill of that order panics
/// with "attempt to subtract with overflow" at
/// `maker_state.open_order_count -= 1`.
///
/// Scenario:
///  1. User1 goes long 5 ETH @ $2,000.
///  2. User1 places a resting bid at $1,950 (intending to add to the long).
///  3. User1 places a stop-loss BELOW $1,960, size -5, slippage 5%.
///     Acceptable bids at trigger: ≥ $1,960 × 0.95 = $1,862, so User1's own
///     bid at $1,950 is in range.
///  4. Oracle drops to $1,960 → SL triggers → market sell.
///  5. `match_order` iterates bids, hits User1's own bid first → STP cancels it
///     in-memory (decrement count + release reserved margin), but no other bids
///     are in range → `unfilled == fillable_size` → `execute_order` returns
///     `Err("no liquidity at acceptable price")`.
///  6. With the fix: the pre-call snapshot of `user_state` is restored, so
///     `open_order_count == 1` and the resting bid is still in the book.
///  7. A subsequent market sell from User3 fills User1's bid without panicking.
#[tokio::test]
async fn conditional_order_self_trade_failure_preserves_user_state() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Deposits.
    for (user, amount) in [
        (&mut accounts.user1, 10_000_000_000u128),
        (&mut accounts.user2, 100_000_000_000u128),
        (&mut accounts.user3, 10_000_000_000u128),
    ] {
        suite
            .execute(
                user,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(amount)).unwrap(),
            )
            .await
            .should_succeed();
    }

    // Maker (user2) places ask @ $2,000 so User1 can open a long.
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

    // User1 market-buys 5 ETH → long 5 @ $2,000.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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

    // User1 places a resting bid at $1,950 (would add to long if filled).
    // This is the "self-trade bait" that the stop-loss market sell will hit.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_950),
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

    // Snapshot User1's state: should have 1 resting bid, reserved_margin > 0.
    let state_before: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(
        state_before.open_order_count, 1,
        "User1 should have 1 resting bid before the stop-loss triggers"
    );
    let reserved_before = state_before.reserved_margin;
    assert!(
        reserved_before.is_non_zero(),
        "User1's reserved_margin should be non-zero before trigger"
    );

    // User1 places a stop-loss BELOW $1,960, size -5, 5% slippage.
    // At trigger, acceptable bids are ≥ $1,862 — User1's own bid at $1,950
    // is in range, so STP will fire on it.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-5)),
                trigger_price: UsdPrice::new_int(1_960),
                trigger_direction: TriggerDirection::Below,
                max_slippage: Dimensionless::new_percent(5),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Oracle drops to $1,960 → SL triggers. No other bids in range ($1,862+) —
    // only User1's own bid at $1,950. Self-trade prevention cancels it in
    // memory, `match_order` has no more liquidity, `ensure!` fails, and
    // `process_triggered_order` gracefully cancels the conditional order via
    // `SlippageExceeded`.
    register_oracle_prices(&mut suite, &mut accounts, 1_960).await;
    suite.increase_time(Duration::from_minutes(2)).await;

    // --- Post-trigger assertions ---

    let state_after: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    // Position unchanged (market sell did not actually fill).
    assert_eq!(
        state_after.positions.get(&pair).unwrap().size,
        Quantity::new_int(5),
        "User1 should still be long 5 (sell failed to fill)"
    );

    // Stop-loss conditional order is cleared (graceful cancel).
    assert!(
        state_after
            .positions
            .get(&pair)
            .unwrap()
            .conditional_order_below
            .is_none(),
        "User1's stop-loss should be cleared after graceful cancel"
    );

    // CRITICAL: open_order_count and reserved_margin must be restored to their
    // pre-call values — otherwise the in-memory STP decrement leaked into
    // storage and the invariant with BIDS/ASKS is broken.
    assert_eq!(
        state_after.open_order_count, 1,
        "open_order_count must be restored after failed conditional order"
    );
    assert_eq!(
        state_after.reserved_margin, reserved_before,
        "reserved_margin must be restored after failed conditional order"
    );

    // User1's resting bid must still be on the book.
    let orders: std::collections::BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(
        orders.len(),
        1,
        "User1's resting bid must still be on the book after the failed SL"
    );

    // Finally, User3 market-sells 1 ETH which must fill User1's still-resting
    // bid without panicking on `open_order_count -= 1` underflow.
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-1),
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

    // After the fill, User1's resting bid should be gone and
    // open_order_count == 0.
    let state_final: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(
        state_final.open_order_count, 0,
        "open_order_count should be 0 after the resting bid is filled"
    );
}

// ===================== Child order e2e tests ================================

/// Market buy with TP child order → position has conditional_order_above →
/// oracle rises → cron triggers TP → position closed with profit.
#[tokio::test]
async fn child_order_market_with_tp_triggers() {
    let (mut suite, mut accounts, _codes, contracts, _mock_validators) =
        setup_test_naive(TestOption::default());

    let pair = pair_id();
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // Deposit for user1 (trader) and user2 (maker).
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

    // Maker places ask: 10 ETH @ $2,000.
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

    // Trader market buys 10 ETH with TP @ $2,500.
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
                tp: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(2_500),
                    max_slippage: Dimensionless::new_percent(1),
                    size: None,
                }),
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify TP is on the position.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state.positions.get(&pair).expect("should have position");
    assert!(pos.conditional_order_above.is_some(), "TP should be set");
    assert!(pos.conditional_order_below.is_none(), "no SL");

    // Oracle rises to $2,500 → trigger TP.
    register_oracle_prices(&mut suite, &mut accounts, 2_500).await;

    // Maker places bid to fill the TP market sell.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_500),
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

    // Advance time to trigger cron.
    suite.increase_time(Duration::from_minutes(2)).await;

    // Verify position is closed.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        !state.positions.contains_key(&pair),
        "position should be closed by TP"
    );

    // Margin should reflect profit: entry $2,000, exit $2,500, 10 ETH → $5,000 profit.
    assert!(state.margin > UsdValue::new_int(14_000));
}

/// Market buy with SL child order → oracle drops → SL triggers → closed with loss.
#[tokio::test]
async fn child_order_market_with_sl_triggers() {
    let (mut suite, mut accounts, _codes, contracts, _mock_validators) =
        setup_test_naive(TestOption::default());

    let pair = pair_id();
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

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

    // Maker places ask: 5 ETH @ $2,000.
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

    // Trader market buys 5 ETH with SL @ $1,800.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: None,
                sl: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(1_800),
                    max_slippage: Dimensionless::new_percent(2),
                    size: None,
                }),
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify SL is on the position.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state.positions.get(&pair).unwrap();
    assert!(pos.conditional_order_below.is_some(), "SL should be set");
    assert!(pos.conditional_order_above.is_none(), "no TP");

    // Oracle drops to $1,800 → trigger SL.
    register_oracle_prices(&mut suite, &mut accounts, 1_800).await;

    // Maker places bid to fill SL.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_800),
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

    suite.increase_time(Duration::from_minutes(2)).await;

    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(
        !state.positions.contains_key(&pair),
        "position closed by SL"
    );
    assert!(state.margin < UsdValue::new_int(9_100)); // loss
}

/// Market sell that closes existing long position, with TP/SL child order →
/// no conditional orders remain.
#[tokio::test]
async fn child_order_ignored_when_position_closed() {
    let (mut suite, mut accounts, _codes, contracts, _mock_validators) =
        setup_test_naive(TestOption::default());

    let pair = pair_id();
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

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

    // Maker places ask, trader buys to establish a long.
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

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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

    // Now maker places bid, trader sells to close with TP/SL attached.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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
                size: Quantity::new_int(-5), // close the long
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(1_800),
                    max_slippage: Dimensionless::new_percent(1),
                    size: None,
                }),
                sl: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(2_200),
                    max_slippage: Dimensionless::new_percent(2),
                    size: None,
                }),
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Position should be closed, no conditional orders.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert!(!state.positions.contains_key(&pair));
}

/// Position has existing TP/SL. New market order with different child orders fills
/// → old TP/SL replaced.
#[tokio::test]
async fn child_order_overwrites_existing() {
    let (mut suite, mut accounts, _codes, contracts, _mock_validators) =
        setup_test_naive(TestOption::default());

    let pair = pair_id();
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

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

    // Establish position.
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
                size: Quantity::new_int(5),
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

    // Set existing TP/SL via SubmitConditionalOrder.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: None,
                trigger_price: UsdPrice::new_int(3_000),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(1),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Buy more with different TP/SL child orders → overwrites.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_percent(50),
                },
                reduce_only: false,
                tp: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(2_500),
                    max_slippage: Dimensionless::new_percent(1),
                    size: None,
                }),
                sl: Some(ChildOrder {
                    trigger_price: UsdPrice::new_int(1_800),
                    max_slippage: Dimensionless::new_percent(2),
                    size: None,
                }),
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state.positions.get(&pair).unwrap();

    // Old TP was at $3,000, new should be at $2,500.
    let above = pos.conditional_order_above.as_ref().unwrap();
    assert_eq!(above.trigger_price, UsdPrice::new_int(2_500));

    // SL (new) should be set.
    let below = pos.conditional_order_below.as_ref().unwrap();
    assert_eq!(below.trigger_price, UsdPrice::new_int(1_800));
}

/// SubmitConditionalOrder twice with same direction → second overwrites first.
#[tokio::test]
async fn conditional_order_overwrite_same_direction() {
    let (mut suite, mut accounts, _codes, contracts, _mock_validators) =
        setup_test_naive(TestOption::default());

    let pair = pair_id();
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

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

    // Establish long position.
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

    // First TP.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: None,
                trigger_price: UsdPrice::new_int(2_500),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(1),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Second TP (same direction) → should overwrite, not error.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: None,
                trigger_price: UsdPrice::new_int(3_000),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(1),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state.positions.get(&pair).unwrap();
    let above = pos.conditional_order_above.as_ref().unwrap();
    assert_eq!(
        above.trigger_price,
        UsdPrice::new_int(3_000),
        "should be overwritten to $3,000"
    );
}

/// SubmitConditionalOrder with size > position → now allowed (previously errored).
#[tokio::test]
async fn conditional_order_size_exceeds_position_allowed() {
    let (mut suite, mut accounts, _codes, contracts, _mock_validators) =
        setup_test_naive(TestOption::default());

    let pair = pair_id();
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

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

    // Establish small long.
    suite
        .execute(
            &mut accounts.user2,
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

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(3),
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

    // Submit TP with size > position (was -5 but position is only 3).
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-5)),
                trigger_price: UsdPrice::new_int(2_500),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(1),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Verify it was placed.
    let state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    let pos = state.positions.get(&pair).unwrap();
    let above = pos.conditional_order_above.as_ref().unwrap();
    assert_eq!(above.size, Some(Quantity::new_int(-5)));
}

/// If governance tightens `max_market_slippage` after a conditional order
/// is submitted, and the order's stored `max_slippage` now exceeds the
/// cap, the cron trigger path cancels the order gracefully with
/// `SlippageCapTightened` — distinct from `SlippageExceeded` which
/// signals a book-liquidity shortfall. The position stays open; no
/// market order is attempted.
#[tokio::test]
async fn conditional_order_cancelled_when_slippage_cap_tightened() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Start with a permissive cap so the trader's 5% TP slippage is
    // legal at submission.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: default_param(),
                pair_params: btree_map! {
                    pair.clone() => PairParam {
                        max_market_slippage: Dimensionless::new_permille(500), // 50%
                        ..default_pair_param()
                    },
                },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Trader deposits and opens a long via a market fill against a
    // maker ask.
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
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

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

    // Trader places TP with 10% slippage — legal against the 50% cap.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-10)),
                trigger_price: UsdPrice::new_int(2_500),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(10),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Governance tightens the cap to 5% — the stored 10% TP slippage
    // now exceeds it.
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

    // Provide a bid the TP could legally fill at, so the only reason
    // for failure is the cap mismatch (not absent liquidity).
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(10),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_500),
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

    // Oracle rises to the TP trigger.
    register_oracle_prices(&mut suite, &mut accounts, 2_500).await;

    // Advance time so the perps cron fires.
    suite.increase_time(Duration::from_minutes(2)).await;

    // Position is still open (TP was not executed; order was cancelled
    // for cap tightening).
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
        .expect("position should still be open — TP cancelled, not executed");
    assert_eq!(pos.size, Quantity::new_int(10));
    assert!(
        pos.conditional_order_above.is_none(),
        "conditional order should have been removed by the cap-tightened cancel"
    );
}

/// A TP that fires from cron and crosses a resting maker produces
/// `OrderFilled` events carrying `Some(fill_id)` on both sides of the
/// match. Verifies the cron path of `compute_submit_order_outcome` → `match_order` →
/// `settle_fill` correctly threads `next_fill_id` the same way the
/// user-submitted path does.
#[tokio::test]
async fn conditional_order_trigger_fills_carry_fill_id() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Trader deposits, buys 5 ETH long, attaches a TP at $2,500.
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
            Coins::one(usdc::DENOM.clone(), Uint128::new(50_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

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

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
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

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair.clone(),
                size: Some(Quantity::new_int(-5)),
                trigger_price: UsdPrice::new_int(2_500),
                trigger_direction: TriggerDirection::Above,
                max_slippage: Dimensionless::new_percent(5),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Resting bid that the TP will cross when triggered.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_500),
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

    // Move oracle above the TP trigger and advance time to fire cron.
    // `increase_time` discards the block outcome, so inline its body to
    // keep access to the cron-emitted events.
    register_oracle_prices(&mut suite, &mut accounts, 2_500).await;

    let old_block_time = suite.block_time;
    suite.block_time = Duration::from_minutes(2);
    let outcome = suite.make_empty_block().await;
    suite.block_time = old_block_time;

    let fills = outcome
        .block_outcome
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_filled")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderFilled>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        fills.len(),
        2,
        "one TP crossing one maker should emit two OrderFilled events"
    );

    for filled in &fills {
        assert!(
            filled.fill_id.is_some(),
            "cron-triggered TP fills must carry a fill_id"
        );
    }
    assert_eq!(
        fills[0].fill_id, fills[1].fill_id,
        "the taker and maker sides of a single match must share one fill_id"
    );

    let mut is_makers = fills.iter().map(|f| f.is_maker).collect::<Vec<_>>();
    is_makers.sort();
    assert_eq!(
        is_makers,
        vec![Some(false), Some(true)],
        "the two OrderFilled events of a match must have one is_maker=Some(false) (taker) \
         and one is_maker=Some(true) (maker)"
    );
}

/// Two TP orders that fire in the same `process_conditional_orders`
/// invocation must produce consecutive fill ids. This pins the storage
/// round-trip at `dango/exchange/perps/src/cron/process_conditional_orders.rs`:
/// after the first triggered order saves its advanced `NEXT_FILL_ID`,
/// the second triggered order must load the updated value rather than
/// the pre-cron one.
#[tokio::test]
async fn two_conditional_triggers_in_one_cron_tick_have_consecutive_fill_ids() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // Two traders each open a long position and attach a TP.
    for trader in [&mut accounts.user1, &mut accounts.user3] {
        suite
            .execute(
                trader,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
                Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
            )
            .await
            .should_succeed();
    }

    // Maker deposits and seeds the book with asks so both traders can
    // open their longs, then later provides bids to fill the TPs.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Two opening asks, one per trader.
    for _ in 0..2 {
        suite
            .execute(
                &mut accounts.user2,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(
                    perps::SubmitOrderRequest {
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
                    },
                )),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    for trader in [&mut accounts.user1, &mut accounts.user3] {
        suite
            .execute(
                trader,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(
                    perps::SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(5),
                        kind: OrderKind::Market {
                            max_slippage: Dimensionless::new_percent(50),
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

    // Both traders submit TPs at the same trigger price so both fire in
    // the same cron tick. Using slightly different trigger prices is
    // not required — the cron handler processes every triggered order
    // in a single invocation regardless of relative trigger prices.
    for trader in [&mut accounts.user1, &mut accounts.user3] {
        suite
            .execute(
                trader,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                    pair_id: pair.clone(),
                    size: Some(Quantity::new_int(-5)),
                    trigger_price: UsdPrice::new_int(2_500),
                    trigger_direction: TriggerDirection::Above,
                    max_slippage: Dimensionless::new_percent(5),
                }),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    // Two resting bids — one per TP — priced generously enough to fill.
    for _ in 0..2 {
        suite
            .execute(
                &mut accounts.user2,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(
                    perps::SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(5),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(2_500),
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

    // Move the oracle so both TPs trigger, then fire cron.
    register_oracle_prices(&mut suite, &mut accounts, 2_500).await;

    let old_block_time = suite.block_time;
    suite.block_time = Duration::from_minutes(2);
    let outcome = suite.make_empty_block().await;
    suite.block_time = old_block_time;

    let fills = outcome
        .block_outcome
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_filled")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderFilled>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        fills.len(),
        4,
        "two TPs × two sides per match = four OrderFilled events"
    );

    // Group by fill_id. Must be exactly two distinct values with two
    // events each, and the two values must be consecutive.
    let mut by_fill_id = BTreeMap::<_, usize>::new();
    for filled in &fills {
        let id = filled
            .fill_id
            .expect("cron-triggered fill must carry a fill_id");
        *by_fill_id.entry(id).or_default() += 1;
    }

    assert_eq!(
        by_fill_id.len(),
        2,
        "two independent matches should produce two distinct fill_ids"
    );
    for (id, count) in &by_fill_id {
        assert_eq!(
            *count, 2,
            "fill_id {} should appear on exactly two events (taker + maker); got {}",
            id, count,
        );
    }

    // Each fill_id pair must contain exactly one taker (is_maker=false)
    // and one maker (is_maker=true).
    let mut by_fill_id_makers = BTreeMap::<_, Vec<_>>::new();
    for filled in &fills {
        let id = filled.fill_id.unwrap();
        by_fill_id_makers
            .entry(id)
            .or_default()
            .push(filled.is_maker);
    }
    for (id, mut makers) in by_fill_id_makers {
        makers.sort();
        assert_eq!(
            makers,
            vec![Some(false), Some(true)],
            "fill_id {} must pair one is_maker=Some(false) (taker) with one is_maker=Some(true) \
             (maker); got {:?}",
            id,
            makers,
        );
    }

    let mut ids = by_fill_id.keys().copied().collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        *ids[1].inner(),
        ids[0].inner() + 1,
        "the second cron-triggered match's fill_id must be the first's + 1 \
         (pins the NEXT_FILL_ID storage round-trip between triggers)"
    );
}
