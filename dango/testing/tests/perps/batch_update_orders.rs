use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::{Uint64, Uint128},
    dango_order_book::{
        Dimensionless, OrderId, OrderKind, OrderRemoved, Quantity, QueryOrdersByUserResponseItem,
        ReasonForOrderRemoval, TimeInForce, UsdPrice,
    },
    dango_primitives::{
        Addressable, CheckedContractEvent, Coins, Denom, JsonDeExt, NonEmpty, QuerierExt,
        ResultExt, SearchEvent, btree_map,
    },
    dango_testing::{OracleTestEntry, TestOption, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{
            self, CancelOrderRequest, Param, SubmitOrCancelOrderRequest, SubmitOrderRequest,
            UserReferralData,
        },
    },
    std::collections::BTreeMap,
};

fn limit_bid(price: i128, size: i128, cid: Option<u64>) -> SubmitOrderRequest {
    SubmitOrderRequest {
        pair_id: pair_id(),
        size: Quantity::new_int(size),
        kind: OrderKind::Limit {
            limit_price: UsdPrice::new_int(price),
            time_in_force: TimeInForce::PostOnly,
            client_order_id: cid.map(Uint64::new),
        },
        reduce_only: false,
        tp: None,
        sl: None,
    }
}

fn limit_ask(price: i128, size: i128, cid: Option<u64>) -> SubmitOrderRequest {
    SubmitOrderRequest {
        pair_id: pair_id(),
        size: Quantity::new_int(-size),
        kind: OrderKind::Limit {
            limit_price: UsdPrice::new_int(price),
            time_in_force: TimeInForce::PostOnly,
            client_order_id: cid.map(Uint64::new),
        },
        reduce_only: false,
        tp: None,
        sl: None,
    }
}

/// Happy path: batch of [PostOnly bid, PostOnly ask, Cancel(One(bid))].
/// Assert exactly one resting order remains (the ask).
#[tokio::test]
async fn batch_submit_then_cancel() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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

    // Submit bid first to learn its OrderId, then run the batch.
    // Easier: use client_order_id on the bid to reference it in the cancel.
    let bid_cid = 1u64;
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_900, 1, Some(bid_cid))),
                    SubmitOrCancelOrderRequest::Submit(limit_ask(2_100, 1, None)),
                    SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::OneByClientOrderId(
                        Uint64::new(bid_cid),
                    )),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(orders.len(), 1, "only the ask should remain on the book");
    let (_, ask) = orders.iter().next().unwrap();
    assert_eq!(ask.size, Quantity::new_int(-1));
}

/// Atomic replacement: user rests 3 orders, then issues a batch that
/// cancels all and re-submits 3 new ones. Assert the new orders are on
/// the book and the old ones are gone.
#[tokio::test]
async fn batch_atomic_replace() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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

    // Rest 3 bids at descending prices.
    for price in [1_900, 1_800, 1_700] {
        suite
            .execute(
                &mut accounts.user1,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(limit_bid(price, 1, None))),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    let old_orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(old_orders.len(), 3);
    let old_ids: Vec<OrderId> = old_orders.keys().copied().collect();

    // Batch: cancel all + 3 new submits at different prices.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::All),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_850, 1, None)),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_750, 1, None)),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_650, 1, None)),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    let new_orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(new_orders.len(), 3, "book should hold the 3 new bids");
    for id in old_ids {
        assert!(
            !new_orders.contains_key(&id),
            "old order id {id} should have been canceled"
        );
    }
}

/// Within one batch, a `Cancel(OneByClientOrderId(42))` releases the
/// client id so a subsequent `Submit` carrying the same client id
/// succeeds — the two actions see each other's writes via the engine's
/// in-call `Buffer`.
#[tokio::test]
async fn batch_reuse_client_order_id() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let cid = 42u64;

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Resting bid with client_order_id = 42.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(limit_bid(
                1_900,
                1,
                Some(cid),
            ))),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Batch: cancel by cid, then submit a new order carrying the same cid.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::OneByClientOrderId(
                        Uint64::new(cid),
                    )),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_800, 1, Some(cid))),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(orders.len(), 1);
    let (_, order) = orders.iter().next().unwrap();
    assert_eq!(order.limit_price, UsdPrice::new_int(1_800));
}

/// A duplicate `client_order_id` within a batch fails the second submit
/// and rolls back the first. Snapshot `UserState` and `orders` before
/// the batch; assert they're byte-identical after the failure.
#[tokio::test]
async fn batch_atomicity_on_submit_failure() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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

    let state_before: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let orders_before: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();

    // Two submits with the same client_order_id. The first is accepted by the
    // matching engine, the second collides on the unique-cid index and fails.
    let cid = 7u64;
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_900, 1, Some(cid))),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_800, 1, Some(cid))),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_fail();

    let state_after: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let orders_after: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();

    assert_eq!(state_before, state_after, "UserState must be unchanged");
    assert_eq!(orders_before, orders_after, "orders must be unchanged");
}

/// A bogus `Cancel(One(...))` mid-batch rolls back an earlier successful
/// submit. Same snapshot-and-compare assertion as the submit-failure
/// case above.
#[tokio::test]
async fn batch_atomicity_on_cancel_failure() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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

    let state_before: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let orders_before: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_900, 1, None)),
                    SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::One(Uint64::new(
                        99_999,
                    ))),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_fail_with_error("order not found");

    let state_after: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let orders_after: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();

    assert_eq!(state_before, state_after);
    assert_eq!(orders_before, orders_after);
}

/// Critical atomicity case: a batch fills an existing resting order
/// (mutating the maker's position/margin, emitting `OrderFilled`), then
/// hits an invalid action. The fill itself must be rolled back — not
/// just the resting portion of the taker's message.
///
/// Maker user1 rests an ask. Taker user2 submits
/// [market buy that crosses, submit with duplicate cid]. The duplicate
/// cid fails the second action. After the revert, user1's resting ask
/// is still on the book and user1's UserState (margin, position) is
/// byte-identical to before the batch.
#[tokio::test]
async fn batch_fill_reverts_on_later_failure() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // Both users deposit margin.
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

    // user1 rests an ask at $2,000 for 1 ETH.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(limit_ask(2_000, 1, None))),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Snapshot maker (user1) and taker (user2) state plus their order books
    // before the batch. The taker-side snapshot catches a future bug that
    // would leak taker mutations (position, reserved_margin, unrested bids)
    // past the rollback.
    let user1_state_before: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user1_orders_before: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    let user2_state_before: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user2_orders_before: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    // user2 submits a batch: market buy (crosses user1's ask) followed by a
    // submit whose duplicate client_order_id fails.
    let cid = 99u64;
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(SubmitOrderRequest {
                        pair_id: pair_id(),
                        size: Quantity::new_int(1),
                        kind: OrderKind::Market {
                            max_slippage: Dimensionless::new_percent(50),
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    }),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_900, 1, Some(cid))),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_800, 1, Some(cid))),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_fail();

    // Both maker and taker state must be byte-identical — the fill against
    // user1's ask and any intermediate writes to user2 (margin reservation,
    // a bid resting mid-batch under cid=99) are rolled back along with the
    // rest of the batch.
    let user1_state_after: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user1_orders_after: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    let user2_state_after: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user2_orders_after: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    assert_eq!(
        user1_state_before, user1_state_after,
        "maker UserState must be unchanged — the fill must revert"
    );
    assert_eq!(
        user1_orders_before, user1_orders_after,
        "maker's resting ask must still be on the book"
    );
    assert_eq!(
        user2_state_before, user2_state_after,
        "taker UserState must be unchanged — no position, margin, or \
         reserved_margin mutations may leak past the rollback"
    );
    assert_eq!(
        user2_orders_before, user2_orders_after,
        "taker must have no resting orders — the mid-batch bid at cid=99 \
         is rolled back along with the rest"
    );
}

/// Sanity: a single-action batch produces observable state equivalent
/// to the corresponding direct `SubmitOrder` message.
#[tokio::test]
async fn batch_single_action() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![SubmitOrCancelOrderRequest::Submit(limit_bid(
                    1_900, 1, None,
                ))])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(orders.len(), 1);
}

/// `Param::max_action_batch_size` is enforced. Lower the cap to 3 via
/// `Configure`; a 4-action batch is rejected without touching any
/// storage.
#[tokio::test]
async fn batch_size_cap_enforced() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // Lower the cap to 3.
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    max_action_batch_size: 3,
                    ..default_param()
                },
                pair_params: btree_map! {
                    pair_id() => default_pair_param(),
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

    let state_before: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    // 4-action batch exceeds the cap of 3 → rejected before any action runs.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_900, 1, None)),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_800, 1, None)),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_700, 1, None)),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_600, 1, None)),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_fail_with_error("`max_action_batch_size` (3), found: 4");

    let state_after: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    assert_eq!(state_before, state_after);

    // A 3-action batch at the cap still succeeds.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_900, 1, None)),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_800, 1, None)),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_700, 1, None)),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();
}

/// A batch that touches two pairs (ETH and BTC): post-only bid on
/// ETH with `cid=1`, post-only bid on BTC, and a cancel by
/// `client_order_id` referencing the ETH bid. The cancel only
/// succeeds if the cid-index write from the first action is visible
/// through the in-call `Buffer`, which proves cross-pair reads
/// inside a batch see earlier in-batch writes.
#[tokio::test]
async fn batch_across_two_pairs() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    let eth_pair = pair_id();
    let btc_pair: Denom = "perp/btcusd".parse().unwrap();

    // Register oracle prices for both pairs (plus USDC for settlement).
    suite
        .seed_oracle_prices(
            &mut accounts.owner,
            btree_map! {
                usdc::DENOM.clone() => OracleTestEntry {
                    pyth_id: 1,
                    humanized_price: UsdPrice::new_int(1),
                },
                eth_pair.clone() => OracleTestEntry {
                    pyth_id: 2,
                    humanized_price: UsdPrice::new_int(2_000),
                },
                btc_pair.clone() => OracleTestEntry {
                    pyth_id: 3,
                    humanized_price: UsdPrice::new_int(60_000),
                },
            },
        )
        .await;

    // Add the BTC pair (the ETH pair is already configured at genesis;
    // re-specifying it keeps it unchanged).
    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: default_param(),
                pair_params: btree_map! {
                    eth_pair.clone() => default_pair_param(),
                    btc_pair.clone() => default_pair_param(),
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
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    let eth_cid = 1u64;

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(SubmitOrderRequest {
                        pair_id: eth_pair.clone(),
                        size: Quantity::new_int(1),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(1_900),
                            time_in_force: TimeInForce::PostOnly,
                            client_order_id: Some(Uint64::new(eth_cid)),
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    }),
                    SubmitOrCancelOrderRequest::Submit(SubmitOrderRequest {
                        pair_id: btc_pair.clone(),
                        size: Quantity::new_int(1),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(58_000),
                            time_in_force: TimeInForce::PostOnly,
                            client_order_id: None,
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    }),
                    SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::OneByClientOrderId(
                        Uint64::new(eth_cid),
                    )),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Only the BTC bid remains; the ETH bid was cancelled by its
    // just-written cid.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(orders.len(), 1, "only the BTC bid should remain");
    let (_, order) = orders.iter().next().unwrap();
    assert_eq!(order.pair_id, btc_pair);
    assert_eq!(order.size, Quantity::new_int(1));
    assert_eq!(order.limit_price, UsdPrice::new_int(58_000));
}

/// Self-trade prevention fires when a later batch action crosses a
/// resting order submitted earlier in the same batch. The STP-
/// cancelled order emits an `OrderRemoved` event with reason
/// `SelfTradePrevention` carrying the original `client_order_id`.
/// The taker's remaining GTC ask rests since no other makers exist.
#[tokio::test]
async fn batch_stp_fires_for_self_match() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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

    let bid_cid = 42u64;

    // Batch: rest a post-only bid at $1,900 with cid=42, then submit
    // a GTC ask at $1,800. `match_order` iterates bids best-first,
    // finds user1's just-placed bid, triggers STP (maker_order.user
    // == taker), cancels the bid, and continues. No other bids →
    // unfilled remainder rests as the taker's new ask.
    let events = suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_900, 1, Some(bid_cid))),
                    SubmitOrCancelOrderRequest::Submit(SubmitOrderRequest {
                        pair_id: pair_id(),
                        size: Quantity::new_int(-1),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(1_800),
                            time_in_force: TimeInForce::GoodTilCanceled,
                            client_order_id: None,
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    }),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_succeed()
        .events;

    let removed = events
        .search_event::<CheckedContractEvent>()
        .with_predicate(|e| e.ty == "order_removed")
        .take()
        .all()
        .into_iter()
        .map(|e| e.event.data.deserialize_json::<OrderRemoved>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(removed.len(), 1, "expected one OrderRemoved event from STP");
    assert_eq!(
        removed[0].reason,
        ReasonForOrderRemoval::SelfTradePrevention
    );
    assert_eq!(removed[0].user, accounts.user1.address());
    assert_eq!(removed[0].client_order_id, Some(Uint64::new(bid_cid)));

    // Post-batch: only the rested ask remains.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(orders.len(), 1, "only the rested ask should remain");
    let (_, order) = orders.iter().next().unwrap();
    assert_eq!(order.size, Quantity::new_int(-1));
    assert_eq!(order.limit_price, UsdPrice::new_int(1_800));
}

/// A batch containing `[Submit(GTC that fully fills),
/// Cancel(OneByClientOrderId(same cid))]` reverts. The submit's cid
/// is never persisted because unfilled=0 means no `order_to_store`,
/// so the cancel's cid-index lookup returns "order not found".
/// Both maker and taker state must be byte-identical to the
/// pre-batch snapshot.
#[tokio::test]
async fn batch_cancel_fails_for_filled_order() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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

    // user2 rests a post-only ask at $2,000 for user1 to fully fill.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(limit_ask(2_000, 1, None))),
            Coins::new(),
        )
        .await
        .should_succeed();

    let user1_state_before: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user2_state_before: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user2_orders_before: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    let cid = 99u64;

    // user1's GTC bid at $2,000 size=1 fully crosses user2's resting
    // ask at $2,000 (unfilled=0, so no order_to_store). The later
    // Cancel(OneByClientOrderId) can't resolve cid=99 → "order not
    // found" → batch reverts.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(SubmitOrderRequest {
                        pair_id: pair_id(),
                        size: Quantity::new_int(1),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(2_000),
                            time_in_force: TimeInForce::GoodTilCanceled,
                            client_order_id: Some(Uint64::new(cid)),
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    }),
                    SubmitOrCancelOrderRequest::Cancel(CancelOrderRequest::OneByClientOrderId(
                        Uint64::new(cid),
                    )),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_fail_with_error("order not found");

    // Full rollback: maker's resting ask and both users' states are
    // byte-identical to the snapshot.
    let user1_state_after: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user2_state_after: perps::UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let user2_orders_after: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed();

    assert_eq!(user1_state_before, user1_state_after);
    assert_eq!(user2_state_before, user2_state_after);
    assert_eq!(user2_orders_before, user2_orders_after);
}

/// A batch whose earlier action triggers `apply_fee_commissions`
/// (a realized fill that writes to the referral tables) must roll
/// back every referral-table write if a later action fails.
///
/// Setup: user1 becomes a referrer, user2 sets user1 as referrer,
/// user3 rests an ask. user2 submits a batch `[market buy that
/// fills user3's ask, post-only bid cid=7, post-only bid cid=7]`.
/// The third action collides on the cid and fails → batch reverts.
/// `USER_REFERRAL_DATA` for both referrer and referee must be
/// unchanged.
#[tokio::test]
async fn batch_referral_commissions_rollback() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // user1 activates their referrer slot by setting a fee share
    // ratio; user2 then points to user1 as their referrer.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetFeeShareRatio {
                share_ratio: Dimensionless::new_percent(50),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Referral(perps::ReferralMsg::SetReferral {
                referrer: 1,
                referee: 2,
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // user2 (taker/payer) and user3 (maker/counterparty) deposit.
    for user in [&mut accounts.user2, &mut accounts.user3] {
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
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(limit_ask(2_000, 1, None))),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Snapshot cumulative referral data for both referrer and referee
    // before the failing batch.
    let user1_data_before: UserReferralData = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferralDataRequest {
                user: 1,
                since: None,
            },
        )
        .should_succeed();
    let user2_data_before: UserReferralData = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferralDataRequest {
                user: 2,
                since: None,
            },
        )
        .should_succeed();

    let cid = 7u64;

    // Action 1 fills user3's ask, which invokes apply_fee_commissions
    // and writes to USER_REFERRAL_DATA for both user1 and user2.
    // Action 2 rests a bid with cid=7. Action 3 tries to rest
    // another bid with the same cid and fails the UniqueIndex check,
    // reverting the entire batch.
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new(vec![
                    SubmitOrCancelOrderRequest::Submit(SubmitOrderRequest {
                        pair_id: pair_id(),
                        size: Quantity::new_int(1),
                        kind: OrderKind::Market {
                            max_slippage: Dimensionless::new_percent(10),
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    }),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_800, 1, Some(cid))),
                    SubmitOrCancelOrderRequest::Submit(limit_bid(1_700, 1, Some(cid))),
                ])
                .unwrap(),
            )),
            Coins::new(),
        )
        .await
        .should_fail();

    let user1_data_after: UserReferralData = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferralDataRequest {
                user: 1,
                since: None,
            },
        )
        .should_succeed();
    let user2_data_after: UserReferralData = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryReferralDataRequest {
                user: 2,
                since: None,
            },
        )
        .should_succeed();

    assert_eq!(
        user1_data_before, user1_data_after,
        "referrer's cumulative referral data must be unchanged after batch rollback"
    );
    assert_eq!(
        user2_data_before, user2_data_after,
        "referee's cumulative referral data must be unchanged after batch rollback"
    );
}

/// The `orders_by_user` query is deliberately unpaginated: a user's resting
/// orders are bounded by `Param::max_open_orders`, and the query returns all
/// of them, however many there are.
#[tokio::test]
async fn orders_by_user_returns_all_orders() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());
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

    // 35 resting bids — more than `DEFAULT_PAGE_LIMIT` (30), the fallback
    // most enumerative queries would truncate at — submitted in batches of
    // `max_action_batch_size` (5), at distinct prices within the price band.
    for batch in 0..7i128 {
        let submits = (0..5i128)
            .map(|i| SubmitOrCancelOrderRequest::Submit(limit_bid(1_200 + batch * 5 + i, 1, None)))
            .collect::<Vec<_>>();

        suite
            .execute(
                &mut accounts.user1,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                    NonEmpty::new(submits).unwrap(),
                )),
                Coins::new(),
            )
            .await
            .should_succeed();
    }

    let all: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(all.len(), 35);
}
