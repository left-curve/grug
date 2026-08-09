use {
    crate::register_oracle_prices,
    dango_math::{Uint64, Uint128},
    dango_order_book::{
        ClientOrderId, OrderId, OrderKind, Quantity, QueryOrderByClientOrderIdResponse,
        QueryOrdersByUserResponseItem, TimeInForce, UsdPrice,
    },
    dango_primitives::{Addressable, Coins, QuerierExt, ResultExt},
    dango_testing::{TestOption, pair_id, setup_test_naive},
    dango_types::{constants::usdc, perps},
    std::collections::BTreeMap,
};

/// End-to-end: submit a GTC limit order carrying a `client_order_id`,
/// cancel it via `CancelOrderRequest::OneByClientOrderId`, and prove the
/// same `client_order_id` is reusable after the cancel.
///
/// Covers the full execute-message round trip the algo-trader use case
/// depends on.
#[tokio::test]
async fn submit_cancel_resubmit_by_client_order_id() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();
    let cid: ClientOrderId = Uint64::new(42);

    // Deposit margin so the trader can place a resting order.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Submit a resting GTC limit bid carrying `cid`. The price ($1,500) is
    // well below the oracle ($2,000) so the order rests rather than crosses.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_500),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: Some(cid),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_succeed();

    // Sanity: exactly one resting order, carrying `cid`.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(orders.len(), 1);

    // Cancel it by the client order id — the trader doesn't need to know
    // the system-assigned `OrderId`.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::CancelOrder(
                perps::CancelOrderRequest::OneByClientOrderId(cid),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    // The order is gone.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert!(orders.is_empty(), "order should be removed after cancel");

    // The same `cid` is reusable now that the prior order is no longer
    // active. (If the alias hadn't been cleared, this would hit
    // `StdError::duplicate_data` from the `client_order_id` UniqueIndex.)
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(5),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_400),
                    time_in_force: TimeInForce::GoodTilCanceled,
                    client_order_id: Some(cid),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
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
    assert_eq!(
        orders.len(),
        1,
        "the re-submitted order should be on the book"
    );
}

/// The `order_by_client_order_id` query: look a resting order up via the
/// `(user, client_order_id)` unique index, on either book side, learning the
/// system-assigned order ID. Misses — an ID the user never used, another
/// user's ID, a canceled order — return `None`.
#[tokio::test]
async fn query_order_by_client_order_id() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();
    let bid_cid: ClientOrderId = Uint64::new(42);
    let ask_cid: ClientOrderId = Uint64::new(43);

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // A resting bid ($1,500, below the $2,000 oracle price) and a resting ask
    // ($2,500, above it), each carrying a client order ID.
    for (size, price, cid) in [(5, 1_500, bid_cid), (-5, 2_500, ask_cid)] {
        suite
            .execute(
                &mut accounts.user1,
                contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(
                    perps::SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(size),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(price),
                            time_in_force: TimeInForce::GoodTilCanceled,
                            client_order_id: Some(cid),
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

    // Cross-check the system-assigned order IDs against the by-user query.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert_eq!(orders.len(), 2);

    // The bid is found, with the correct system order ID and the un-inverted
    // limit price.
    let bid: QueryOrderByClientOrderIdResponse = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrderByClientOrderIdRequest {
                user: accounts.user1.address(),
                client_order_id: bid_cid,
            },
        )
        .should_succeed()
        .expect("the bid should be found by its client order ID");
    let bid_item = orders
        .get(&bid.order_id)
        .expect("the returned order ID should be one of the user's orders");
    assert_eq!(bid_item.client_order_id, Some(bid_cid));
    assert_eq!(bid.pair_id, pair);
    assert_eq!(bid.size, Quantity::new_int(5));
    assert_eq!(bid.limit_price, UsdPrice::new_int(1_500));
    assert_eq!(bid.created_at, bid_item.created_at);

    // The ask is found too — covers the `ASKS` branch of the lookup.
    let ask: QueryOrderByClientOrderIdResponse = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrderByClientOrderIdRequest {
                user: accounts.user1.address(),
                client_order_id: ask_cid,
            },
        )
        .should_succeed()
        .expect("the ask should be found by its client order ID");
    assert_eq!(
        orders
            .get(&ask.order_id)
            .expect("the returned order ID should be one of the user's orders")
            .client_order_id,
        Some(ask_cid),
    );
    assert_eq!(ask.size, Quantity::new_int(-5));
    assert_eq!(ask.limit_price, UsdPrice::new_int(2_500));

    // A client order ID the user never used: `None`.
    suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrderByClientOrderIdRequest {
                user: accounts.user1.address(),
                client_order_id: Uint64::new(99),
            },
        )
        .should_succeed_and(Option::is_none);

    // The index is scoped per sender: another user querying the same client
    // order ID gets `None`.
    suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrderByClientOrderIdRequest {
                user: accounts.user2.address(),
                client_order_id: bid_cid,
            },
        )
        .should_succeed_and(Option::is_none);

    // After canceling the bid, the lookup returns `None`.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::CancelOrder(
                perps::CancelOrderRequest::OneByClientOrderId(bid_cid),
            )),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrderByClientOrderIdRequest {
                user: accounts.user1.address(),
                client_order_id: bid_cid,
            },
        )
        .should_succeed_and(Option::is_none);
}

/// Cancelling a `client_order_id` that the sender never used (or has
/// already cancelled / had filled) bails with a clear error message.
#[tokio::test]
async fn cancel_by_unknown_client_order_id_fails() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::CancelOrder(
                perps::CancelOrderRequest::OneByClientOrderId(Uint64::new(99)),
            )),
            Coins::new(),
        )
        .await
        .should_fail_with_error("order not found");
}
