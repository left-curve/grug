//! Price-banding tests.
//!
//! All tests in this module use a 10% band (`max_limit_price_deviation = 0.1`)
//! with oracle = $2,000. Allowed range is [$1,800, $2,200].

use {
    crate::{default_pair_param, default_param, register_oracle_prices},
    dango_math::Uint128,
    dango_order_book::{
        Dimensionless, OrderId, OrderKind, Quantity, QueryOrdersByUserResponseItem, TimeInForce,
        UsdPrice,
    },
    dango_primitives::{Addressable, Coins, QuerierExt, ResultExt, btree_map},
    dango_testing::{TestOption, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, PairParam, UserState},
    },
    std::collections::BTreeMap,
};

/// Set up a test suite with oracle = $2,000, band = 10%, and user1 funded
/// with $10,000 margin.
macro_rules! setup_band_suite {
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
                            max_limit_price_deviation: Dimensionless::new_permille(100), // 10%
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

/// Send a limit order from user1 at the given price and time-in-force.
macro_rules! submit_limit {
    ($suite:expr, $accounts:expr, $contracts:expr, $pair:expr, $size:expr, $price:expr, $tif:expr) => {
        $suite
            .execute(
                &mut $accounts.user1,
                $contracts.perps,
                &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(
                    perps::SubmitOrderRequest {
                        pair_id: $pair.clone(),
                        size: Quantity::new_int($size),
                        kind: OrderKind::Limit {
                            limit_price: UsdPrice::new_int($price),
                            time_in_force: $tif,
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
    };
}

/// GTC limit buy exactly at the upper band bound is accepted.
#[tokio::test]
async fn banding_gtc_at_upper_bound_accepted() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    // oracle * (1 + 10%) = $2,200.
    submit_limit!(
        suite,
        accounts,
        contracts,
        pair,
        1,
        2_200,
        TimeInForce::GoodTilCanceled
    )
    .should_succeed();
}

/// GTC limit buy just above the upper bound is rejected.
#[tokio::test]
async fn banding_gtc_just_above_upper_bound_rejected() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    submit_limit!(
        suite,
        accounts,
        contracts,
        pair,
        1,
        2_201,
        TimeInForce::GoodTilCanceled
    )
    .should_fail_with_error("deviates too far");
}

/// GTC limit sell below the lower bound is rejected.
#[tokio::test]
async fn banding_gtc_below_lower_bound_rejected() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    submit_limit!(
        suite,
        accounts,
        contracts,
        pair,
        -1,
        1_799,
        TimeInForce::GoodTilCanceled
    )
    .should_fail_with_error("deviates too far");
}

/// IOC limit with out-of-band price is rejected before any fill is attempted.
#[tokio::test]
async fn banding_ioc_out_of_band_rejected() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    submit_limit!(
        suite,
        accounts,
        contracts,
        pair,
        1,
        3_000,
        TimeInForce::ImmediateOrCancel
    )
    .should_fail_with_error("deviates too far");
}

/// PostOnly with out-of-band price is rejected at Step 0, never reaching the
/// crossing check inside `store_post_only_limit_order`.
#[tokio::test]
async fn banding_post_only_out_of_band_rejected() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    submit_limit!(
        suite,
        accounts,
        contracts,
        pair,
        -1,
        1_000,
        TimeInForce::PostOnly
    )
    .should_fail_with_error("deviates too far");
}

/// Market orders use `max_slippage`, not `limit_price`, so the band does not
/// apply. A market order with wide slippage against an empty book fails only
/// for lack of liquidity, not for banding reasons.
#[tokio::test]
async fn banding_does_not_affect_market_orders() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(1),
                kind: OrderKind::Market {
                    max_slippage: Dimensionless::new_permille(500), // 50%
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

/// Attack trap-setting regression. An attacker attempting to rest a PostOnly
/// bid far below oracle — the precondition for the bad-debt-minting self-match
/// attack — is rejected at submission. Without banding, this order would rest
/// on the book and serve as the bad-price maker in the attack.
#[tokio::test]
async fn banding_blocks_pathological_post_only_trap() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    // Oracle = $2,000, band = 10%. A PostOnly bid at $200 (90% below
    // oracle) would be the trap leg of a bad-debt-minting attack.
    submit_limit!(
        suite,
        accounts,
        contracts,
        pair,
        1,
        200,
        TimeInForce::PostOnly
    )
    .should_fail_with_error("deviates too far");
}

/// Drift variant: a maker order placed within the band at T1 falls outside
/// the band at T2 after the oracle moves. The match-time re-check cancels
/// the stale order when another user tries to match against it.
///
/// Without this check, an attacker could place a legal PostOnly at T1 and
/// wait for natural or nudged oracle drift to make the resting price
/// pathological, reproducing the bad-debt attack despite submission-time
/// banding.
///
/// Test shape: the walk encounters the stale maker first (near-end drift),
/// cancels it, and then fills against an in-band maker deeper in the book.
/// This verifies `continue` semantics (walk past the cancelled maker,
/// rather than `break`-ing and leaving in-band liquidity unreached).
#[tokio::test]
async fn banding_drift_maker_cancelled_at_match_time() {
    let (mut suite, mut accounts, contracts, pair) = setup_band_suite!();

    // Oracle at T1 = $2,000, band = 10% → allowed range [$1,800, $2,200].
    //
    // user1 (to-be-stale) places PostOnly ask at $2,199 (in-band at T1;
    // will be below the lower bound after oracle moves up).
    submit_limit!(
        suite,
        accounts,
        contracts,
        pair,
        -1,
        2_199,
        TimeInForce::PostOnly
    )
    .should_succeed();

    // Oracle moves to $2,500 → new allowed range [$2,250, $2,750].
    //   - user1's $2,199 ask: now below the lower bound ($2,199 < $2,250).
    //     OUT of band.
    register_oracle_prices(&mut suite, &mut accounts, 2_500).await;

    // user3 deposits and (at T2, in the new band) places a PostOnly ask
    // at $2,400 — this one stays in-band.
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(10_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();
    suite
        .execute(
            &mut accounts.user3,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair.clone(),
                size: Quantity::new_int(-1),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_400),
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

    // user2 market-buys size 2 with wide slippage. Walks asks ascending:
    //   1. user1 at $2,199 (out-of-band) → drift-cancel, continue.
    //   2. user3 at $2,400 (in-band) → fills 1. remaining 1.
    //   3. No more asks → partial fill; tx succeeds.
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
                size: Quantity::new_int(2),
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

    // user1's stale ask was cancelled by the match-time check.
    let orders: BTreeMap<OrderId, QueryOrdersByUserResponseItem> = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryOrdersByUserRequest {
                user: accounts.user1.address(),
            },
        )
        .should_succeed();
    assert!(
        orders.is_empty(),
        "user1's stale ask should have been cancelled by the drift check"
    );

    // user2 filled 1 unit against user3 at $2,400. The stale $2,199 ask
    // was cancelled without filling, so user2's size-2 order is only
    // half-filled.
    let user2_state: UserState = suite
        .query_wasm_smart(
            contracts.perps,
            perps::QueryUserStateRequest {
                user: accounts.user2.address(),
            },
        )
        .should_succeed()
        .unwrap();
    let pos = user2_state
        .positions
        .get(&pair)
        .expect("user2 has a long position");
    assert_eq!(pos.size, Quantity::new_int(1));
    assert_eq!(pos.entry_price, UsdPrice::new_int(2_400));
}
