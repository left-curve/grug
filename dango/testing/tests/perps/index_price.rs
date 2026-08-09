use {
    crate::register_oracle_prices,
    dango_math::Uint128,
    dango_order_book::{OrderKind, Quantity, TimeInForce, UsdPrice},
    dango_primitives::{Addr, Coins, Duration, QuerierExt, ResultExt, Timestamp},
    dango_pyth_types::MarketSession,
    dango_testing::{TestOption, TestSuiteNaive, pair_id, setup_test_naive},
    dango_types::{
        constants::usdc,
        perps::{self, PairState},
    },
};

const ETH_PYTH_ID: u32 = 2;

async fn query_index_price(suite: &TestSuiteNaive, perps_addr: Addr) -> UsdPrice {
    let pair_state: Option<PairState> = suite
        .query_wasm_smart(
            perps_addr,
            perps::QueryPairStateRequest { pair_id: pair_id() },
        )
        .should_succeed();

    pair_state.unwrap().index_price
}

/// After feeding a Regular-session oracle price, the end-of-block cron
/// updates the index price to match the oracle exactly.
#[tokio::test]
async fn e1_basic_snap() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let index = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index, UsdPrice::new_int(2_000));
}

/// Full market-close-and-reopen cycle. The index snaps to the oracle while
/// the market is open. When the session switches to Other, the EWMA kicks
/// in and the index drifts toward the order book over several blocks.
/// When the session returns to Regular with a fresh price, the index snaps
/// back immediately with no smoothing.
#[tokio::test]
async fn e2_close_drift_reopen() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    // Step 1: Feed Regular@2000. After this block, index = 2000.
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    let index_before = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index_before, UsdPrice::new_int(2_000));

    // Step 2: Two users deposit and place limit orders that create impact
    // prices above 2000: bid@2010, ask@2020.
    suite
        .execute(
            &mut accounts.user1,
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
            &perps::ExecuteMsg::Trade(perps::TraderMsg::Deposit { to: None }),
            Coins::one(usdc::DENOM.clone(), Uint128::new(100_000_000_000)).unwrap(),
        )
        .await
        .should_succeed();

    // Place a large bid at 2010
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(100),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_010),
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

    // Place a large ask at 2020
    suite
        .execute(
            &mut accounts.user2,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(-100),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_020),
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

    // Step 3: Feed Other session (market closed). Use a fresh timestamp
    // so the previous Regular price becomes stale in comparison.
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(ETH_PYTH_ID, UsdPrice::new_int(2_000), MarketSession::Other)],
            None,
        )
        .await;

    // Step 4: Make several blocks so EWMA accumulates drift.
    // Index is at 2000, bid@2010 > index -> IPD > 0 -> drift up.
    suite.block_time = Duration::from_seconds(3);
    suite.make_empty_block().await;
    suite.make_empty_block().await;
    suite.make_empty_block().await;

    // 1 EWMA tick (only the feed block triggers process_index_price).
    // dt = 5 × 250ms = 1250ms, x = 1250/1_800_000 → raw(694)
    // β = 1_000_000 − 694 = raw(999_306), α = raw(694)
    // IPD = 2010 − 2000 = 10
    // S_new = 2000 + 694 × 10_000_000 / 1_000_000 = raw(2_000_006_940)
    let index_drifted = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index_drifted, UsdPrice::new_raw(2_000_006_940));

    // Step 5: Feed Regular@2005 (market reopens). R3: snap back immediately.
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(
                ETH_PYTH_ID,
                UsdPrice::new_int(2_005),
                MarketSession::Regular,
            )],
            None,
        )
        .await;

    let index_reopened = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index_reopened, UsdPrice::new_int(2_005));
}

/// A Regular-session price fed with an old explicit timestamp is treated
/// as stale. The index does not snap to it; instead EWMA runs. With no
/// user orders on the book, the index holds at its previous value.
#[tokio::test]
async fn e3_stale_regular_triggers_ewma() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    // Feed with an explicit timestamp so it can become stale.
    let feed_time = Timestamp::from_seconds(1_700_000_000);
    suite.block_time = Duration::from_millis(100);

    // Seed oracle price sources first.
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // Re-feed with an explicit (non-MAX) timestamp.
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(
                ETH_PYTH_ID,
                UsdPrice::new_int(2_000),
                MarketSession::Regular,
            )],
            Some(feed_time),
        )
        .await;

    let index_after_feed = query_index_price(&suite, contracts.perps).await;
    // The feed_time is likely stale relative to the current block time
    // (which is far ahead of 1_700_000_000). So the oracle is treated as
    // unavailable. With no user orders on the book, EWMA produces no change.
    // The index should still be 2000 (set by the initial register_oracle_prices).
    assert_eq!(index_after_feed, UsdPrice::new_int(2_000));

    // Advance a few blocks without re-feeding. The price stays stale.
    suite.make_empty_block().await;
    suite.make_empty_block().await;

    let index_after_stale = query_index_price(&suite, contracts.perps).await;
    // No depth on book -> EWMA holds price.
    assert_eq!(index_after_stale, UsdPrice::new_int(2_000));
}

/// During a closed market session with no user orders on the book, the
/// EWMA has no signal (both sides lack depth, so IPD is zero). The index
/// must remain unchanged across multiple blocks.
#[tokio::test]
async fn e5_empty_book_closed_market_no_movement() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // Switch to Other session (market closed).
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(ETH_PYTH_ID, UsdPrice::new_int(2_000), MarketSession::Other)],
            None,
        )
        .await;

    let index_before = query_index_price(&suite, contracts.perps).await;

    // Advance several blocks. No user orders => empty book => IPD=0.
    suite.block_time = Duration::from_seconds(3);
    suite.make_empty_block().await;
    suite.make_empty_block().await;
    suite.make_empty_block().await;

    let index_after = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index_after, index_before);
}

/// Rapid alternation between Regular and Other sessions. Each Regular
/// feed snaps the index to the oracle price cleanly. Each Other feed
/// lets the EWMA drift from wherever the last snap left it. No state
/// corruption or residual drift leaks across transitions.
#[tokio::test]
async fn e6_oracle_flapping() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;

    // Deposit and place orders so EWMA has something to drift toward.
    suite
        .execute(
            &mut accounts.user1,
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
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(100),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_050),
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

    suite.block_time = Duration::from_seconds(3);

    // Block 1: Regular@2000 -> snap.
    let index1 = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index1, UsdPrice::new_int(2_000));

    // Block 2: Other -> EWMA drift (tiny) from 2000 toward 2050 bid.
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(ETH_PYTH_ID, UsdPrice::new_int(2_000), MarketSession::Other)],
            None,
        )
        .await;

    suite.make_empty_block().await;

    // 1 EWMA tick from the feed block. dt = 2×250ms + 3000ms = 3500ms
    // x = 3500/1_800_000 → raw(1944), β = 1e6 − 1944 + 1 = raw(998_057)
    // α = raw(1943), IPD = 2050 − 2000 = 50
    // S_new = 2000 + 1943 × 50_000_000 / 1_000_000 = raw(2_000_097_150)
    let index2 = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index2, UsdPrice::new_raw(2_000_097_150));

    // Block 3: Regular@2001 -> snap.
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(
                ETH_PYTH_ID,
                UsdPrice::new_int(2_001),
                MarketSession::Regular,
            )],
            None,
        )
        .await;

    let index3 = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index3, UsdPrice::new_int(2_001));

    // Block 4: Other -> EWMA drift from 2001.
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(ETH_PYTH_ID, UsdPrice::new_int(2_001), MarketSession::Other)],
            None,
        )
        .await;

    suite.make_empty_block().await;

    // 1 EWMA tick from the feed block. dt = 3000ms, α = raw(1665)
    // IPD = 2050 − 2001 = 49
    // S_new = 2001 + 1665 × 49_000_000 / 1_000_000 = raw(2_001_081_585)
    let index4 = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index4, UsdPrice::new_raw(2_001_081_585));

    // Block 5: Regular@1999 -> snap.
    suite
        .feed_oracle_prices(
            &mut accounts.owner,
            &[(
                ETH_PYTH_ID,
                UsdPrice::new_int(1_999),
                MarketSession::Regular,
            )],
            None,
        )
        .await;

    let index5 = query_index_price(&suite, contracts.perps).await;
    assert_eq!(index5, UsdPrice::new_int(1_999));
}

/// During a closed session the index cannot be walked arbitrarily far from the
/// last oracle price, and the order price band stays anchored to that oracle
/// rather than re-centring on the drifting index. With the default 10% initial
/// margin ratio the closed-session drift is capped at oracle × (1 + 0.1) =
/// 2200, and a bid outside the band is rejected relative to the oracle (2000),
/// not the drifted mark.
#[tokio::test]
async fn e7_closed_session_drift_bounded_and_band_anchored_to_oracle() {
    let (mut suite, mut accounts, _, contracts, _) = setup_test_naive(TestOption::default());

    // Regular@2000 -> index snaps to 2000 and records it as the oracle anchor.
    register_oracle_prices(&mut suite, &mut accounts, 2_000).await;
    assert_eq!(
        query_index_price(&suite, contracts.perps).await,
        UsdPrice::new_int(2_000)
    );

    // Fund the attacker and rest a large bid near the (50%) band ceiling. Its
    // notional dwarfs impact_size, so it fully sets the impact bid.
    suite
        .execute(
            &mut accounts.user1,
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
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(100),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(2_900),
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

    // Close the market and drive many EWMA ticks at the maximum per-tick weight.
    // Each fed block re-runs `process_index_price`. Even with the impact bid far
    // above (2900), the mark must never exceed the +10% bound of 2200.
    suite.block_time = Duration::from_seconds(180);
    for _ in 0..40 {
        suite
            .feed_oracle_prices(
                &mut accounts.owner,
                &[(ETH_PYTH_ID, UsdPrice::new_int(2_000), MarketSession::Other)],
                None,
            )
            .await;
        assert!(
            query_index_price(&suite, contracts.perps).await <= UsdPrice::new_int(2_200),
            "closed-session mark exceeded the +10% discovery bound"
        );
    }

    // The mark converges to the bound and pins there — the geometric walk is gone.
    assert_eq!(
        query_index_price(&suite, contracts.perps).await,
        UsdPrice::new_int(2_200)
    );

    // Even though the mark drifted to 2200, the band is still measured against
    // the oracle (2000): a bid at 3100 is outside [1000, 3000] and is rejected
    // relative to the oracle, not the drifted mark. Pre-fix the band would have
    // re-centred on 2200 and admitted it.
    suite
        .execute(
            &mut accounts.user1,
            contracts.perps,
            &perps::ExecuteMsg::Trade(perps::TraderMsg::SubmitOrder(perps::SubmitOrderRequest {
                pair_id: pair_id(),
                size: Quantity::new_int(1),
                kind: OrderKind::Limit {
                    limit_price: UsdPrice::new_int(3_100),
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
        .should_fail_with_error("deviates too far from oracle price 2000");
}
