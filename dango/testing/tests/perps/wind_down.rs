use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::Uint128,
    dango_order_book::{OrderKind, Quantity, UsdPrice, UsdValue},
    dango_primitives::{Addressable, Coins, NonEmpty, QuerierExt, ResultExt, btree_map},
    dango_testing::{TestOption, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, Param, SubmitOrCancelOrderRequest, SubmitOrderRequest},
    },
};

/// The error every gated handler must reject with once the exchange has been
/// wound down.
const DISABLED: &str = "trading is disabled";

/// Turning `trading_enabled` off must stop every way of taking on new risk,
/// while leaving every way of getting funds *out* untouched. This is the
/// contract-side half of the wind-down: the chain upgrade flips the flag, and
/// these are the behaviors that flip with it.
#[tokio::test]
async fn disabling_trading_blocks_new_risk_but_not_exits() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

    // ---------------------- Set up while trading is open ---------------------

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // A resting order, so there is something to cancel afterwards.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(1_900),
                    time_in_force: dango_order_book::TimeInForce::GoodTilCanceled,
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

    // --------------------------- Wind trading down ---------------------------

    suite
        .execute(
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    trading_enabled: false,
                    ..default_param()
                },
                pair_params: btree_map! { pair.clone() => default_pair_param() },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    // ------------------------- New risk is rejected --------------------------

    // 1. Placing an order.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Market {
                    max_slippage: dango_order_book::Dimensionless::new_permille(100),
                },
                reduce_only: false,
                tp: None,
                sl: None,
            })),
            Coins::new(),
        )
        .await
        .should_fail_with_error(DISABLED);

    // 2. Placing orders in a batch.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::BatchUpdateOrders(
                NonEmpty::new_unchecked(vec![SubmitOrCancelOrderRequest::Submit(
                    SubmitOrderRequest {
                        pair_id: pair.clone(),
                        size: Quantity::new_int(1),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int(1_900),
                            time_in_force: dango_order_book::TimeInForce::GoodTilCanceled,
                            client_order_id: None,
                        },
                        reduce_only: false,
                        tp: None,
                        sl: None,
                    },
                )]),
            )),
            Coins::new(),
        )
        .await
        .should_fail_with_error(DISABLED);

    // 3. Moving USDC from a spot account into margin.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(1_000_000)).unwrap(),
        )
        .await
        .should_fail_with_error(DISABLED);

    // 4. Moving margin into the counterparty vault.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Vault(perps::VaultMsg::AddLiquidity {
                amount: UsdValue::new_int(100),
                min_shares_to_mint: None,
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error(DISABLED);

    // ------------------------ Getting out still works ------------------------

    // Cancelling a resting order.
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

    // Withdrawing margin back to the spot account.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Withdraw {
                amount: UsdValue::new_int(1_000),
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    let user_state = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed()
        .unwrap();

    assert_eq!(user_state.margin, UsdValue::new_int(9_000));
    assert_eq!(user_state.open_order_count, 0);
}

/// A conditional order is still an order, and it goes through its own handler
/// rather than the one `submit_order` uses, so it needs its own gate.
#[tokio::test]
async fn disabling_trading_blocks_conditional_orders() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let pair = pair_id();

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
            &mut accounts.owner,
            contracts.perps,
            &perps::ExecuteMsg::Maintain(perps::MaintainerMsg::Configure {
                param: Param {
                    trading_enabled: false,
                    ..default_param()
                },
                pair_params: btree_map! { pair.clone() => default_pair_param() },
            }),
            Coins::new(),
        )
        .await
        .should_succeed();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitConditionalOrder {
                pair_id: pair,
                size: None,
                trigger_price: UsdPrice::new_int(1_800),
                trigger_direction: dango_order_book::TriggerDirection::Below,
                max_slippage: dango_order_book::Dimensionless::new_permille(100),
            }),
            Coins::new(),
        )
        .await
        .should_fail_with_error(DISABLED);
}
